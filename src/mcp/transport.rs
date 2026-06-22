//! MCP JSON-RPC Transports (port of `src/mcp/transport.ts`).
//!
//! Two flavors share the same wire format (newline-delimited JSON-RPC 2.0):
//!
//! - [`StdioTransport`] — original transport; reads/writes the process's
//!   stdin/stdout. Used by direct-mode MCP servers.
//! - [`SocketTransport`] — wraps a single Unix-domain socket. Used by the
//!   shared-daemon architecture (see `daemon.rs`) to multiplex multiple MCP
//!   clients onto one CodeGraph instance via per-connection sessions.
//!
//! Both implement [`JsonRpcTransport`] so the session-level protocol logic
//! (initialize / tools/list / tools/call, plus server-initiated `roots/list`)
//! is identical regardless of where the bytes come from.
//!
//! ## Threading model (Rust deviation, behavior-identical)
//!
//! TS rode Node's event loop: incoming lines, handler bodies, and responses
//! to server-initiated requests all interleaved on one thread. Here each
//! transport runs **two** threads:
//!
//! - a *reader* thread that parses incoming lines and routes responses to
//!   pending server-initiated requests directly (so a handler blocked inside
//!   [`JsonRpcTransport::request`] — e.g. `roots/list` mid-`tools/call`, the
//!   exact case the TS socket comment warns about — is unblocked without the
//!   dispatcher's help);
//! - a *dispatcher* thread that invokes the message handler serially, exactly
//!   like handlers ran one-at-a-time on the JS event loop.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use serde_json::{Map, Value};

/// Standard JSON-RPC error codes (TS `ErrorCodes`).
pub struct ErrorCodes;

impl ErrorCodes {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
}

/// An incoming JSON-RPC request or notification (TS `JsonRpcRequest |
/// JsonRpcNotification`). `id: None` means the `id` key was absent — a
/// notification; an explicit JSON `null` id is preserved as `Some(Value::Null)`
/// (TS `'id' in message` semantics).
#[derive(Debug, Clone)]
pub struct IncomingMessage {
    pub id: Option<Value>,
    pub method: String,
    pub params: Option<Value>,
}

impl IncomingMessage {
    /// TS `'id' in message` — whether this is a request (expects a response).
    pub fn is_request(&self) -> bool {
        self.id.is_some()
    }
}

/// Message handler invoked (serially) for every incoming request /
/// notification. An `Err` from a *request* handler is reported back to the
/// client as `Internal error: <msg>` (TS catch in `handleLine`).
pub type MessageHandler = Box<dyn FnMut(IncomingMessage) -> std::result::Result<(), String> + Send>;

/// Reader-thread notification interceptor. Returns `true` when the
/// notification was consumed (it is then NOT queued for the serial
/// dispatcher). Used for `notifications/cancelled`, which must be observed
/// *while* a handler is blocking the dispatcher thread — the analog of rmcp
/// intercepting cancellations in `serve_inner` before task dispatch.
pub type NotificationInterceptor = Box<dyn Fn(&IncomingMessage) -> bool + Send + Sync>;

/// Generic JSON-RPC transport interface — common surface for stdio and socket
/// carriers. Anything below the session layer (initialize, tool dispatch,
/// etc.) talks to this, not to a concrete transport struct.
pub trait JsonRpcTransport: Send + Sync {
    fn start(&self, handler: MessageHandler);
    fn stop(&self);
    /// Install a reader-thread interceptor for incoming *notifications*
    /// (id-less messages). See [`NotificationInterceptor`].
    fn set_notification_interceptor(&self, interceptor: NotificationInterceptor);
    /// Send a pre-built JSON-RPC response value (TS `send`).
    fn send(&self, response: &Value);
    fn notify(&self, method: &str, params: Option<Value>);
    /// Send a server-initiated request to the client and await its response.
    ///
    /// MCP is bidirectional: the server can ask the client questions too. We
    /// use this for `roots/list` — the spec-blessed way to learn the workspace
    /// root when the client didn't pass one in `initialize` (see issue #196).
    /// Errs on timeout so callers can fall back rather than hang forever.
    /// `timeout_ms: None` = the TS default 5000.
    fn request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout_ms: Option<u64>,
    ) -> std::result::Result<Value, String>;
    fn send_result(&self, id: &Value, result: Value);
    fn send_error(&self, id: &Value, code: i64, message: &str, data: Option<Value>);
}

// =============================================================================
// Shared line-based JSON-RPC core (TS `LineBasedJsonRpcTransport`)
// =============================================================================

/// Capacity of the reader→dispatcher queue. EXCEEDS TS: the TS event loop
/// implicitly queues without bound; here a client that pipelines requests
/// faster than the serial handler drains them would otherwise grow memory
/// without limit. On overflow, requests fail fast with an error response and
/// notifications are dropped (see [`queue_or_reject`]). The reader must never
/// *block* on a full queue: the dispatcher may be awaiting a client response
/// (e.g. roots/list) that only the reader can route, so a blocking send could
/// stall the connection until the request timeout.
const MAX_DISPATCH_QUEUE_MESSAGES: usize = 1024;

/// JS truthiness for the `if ('error' in msg && msg.error)` check. Also used
/// by the session layer (`!!value` parity spots).
pub(crate) fn js_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0 && !f.is_nan()).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

pub(crate) struct LineRpcCore {
    /// Outstanding server-initiated requests (e.g. roots/list), keyed by the
    /// id we sent. Responses from the client are matched back here.
    pending: Mutex<HashMap<String, crossbeam_channel::Sender<std::result::Result<Value, String>>>>,
    /// Monotonic per-core counter; ids are `<prefix>-<n>`. Uniqueness relies
    /// on the invariant that each core owns exactly one wire (stdio: one per
    /// process; socket: one per connection) and is never reused across
    /// connections — a reused core or two cores sharing a wire could route a
    /// stale response to the wrong pending request.
    next_request_id: AtomicU64,
    pub(crate) stopped: AtomicBool,
    id_prefix: String,
    /// Writes one line (no trailing newline) to the underlying stream.
    /// Returns `Err` when the underlying write fails (broken pipe, closed
    /// stream) so [`LineRpcCore::request`] can fail fast instead of burning
    /// its full timeout waiting for a response that can never arrive.
    write_line: Box<dyn Fn(&str) -> std::io::Result<()> + Send + Sync>,
    /// Optional reader-thread interceptor for incoming notifications
    /// (`notifications/cancelled` must not queue behind a blocked handler).
    notification_interceptor: Mutex<Option<NotificationInterceptor>>,
}

impl LineRpcCore {
    fn new(
        id_prefix: String,
        write_line: Box<dyn Fn(&str) -> std::io::Result<()> + Send + Sync>,
    ) -> Self {
        LineRpcCore {
            pending: Mutex::new(HashMap::new()),
            next_request_id: AtomicU64::new(1),
            stopped: AtomicBool::new(false),
            id_prefix,
            write_line,
            notification_interceptor: Mutex::new(None),
        }
    }

    fn set_notification_interceptor(&self, interceptor: NotificationInterceptor) {
        *self
            .notification_interceptor
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(interceptor);
    }

    /// Serialize + write one JSON-RPC message. Returns whether the bytes made
    /// it to the underlying stream. Fire-and-forget callers (notify, send_*)
    /// ignore the result — TS parity; `request` uses it to fail fast.
    fn write_value(&self, value: &Value) -> bool {
        match serde_json::to_string(value) {
            Ok(line) => (self.write_line)(&line).is_ok(),
            Err(_) => false,
        }
    }

    fn request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout_ms: Option<u64>,
    ) -> std::result::Result<Value, String> {
        let timeout_ms = timeout_ms.unwrap_or(5000);
        let id = format!(
            "{}-{}",
            self.id_prefix,
            self.next_request_id.fetch_add(1, Ordering::SeqCst)
        );
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), tx);

        let mut map = Map::new();
        map.insert("jsonrpc".to_string(), Value::String("2.0".to_string()));
        map.insert("id".to_string(), Value::String(id.clone()));
        map.insert("method".to_string(), Value::String(method.to_string()));
        if let Some(p) = params {
            map.insert("params".to_string(), p);
        }
        // EXCEEDS TS: a failed write means the response can never arrive —
        // fail fast instead of blocking for the full timeout (a broken pipe
        // used to be indistinguishable from a slow client).
        if !self.write_value(&Value::Object(map)) {
            self.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            return Err(format!(
                "Transport write failed sending \"{method}\" request"
            ));
        }

        match rx.recv_timeout(Duration::from_millis(timeout_ms)) {
            Ok(result) => result,
            Err(_) => {
                self.pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id);
                // EXCEEDS TS: tell the client we abandoned the request so it
                // can stop working on it — mirrors rmcp
                // `RequestHandle::await_response` sending
                // `notifications/cancelled` with `REQUEST_TIMEOUT_REASON`
                // ("request timeout") on its own timeout path. Param shape is
                // rmcp `CancelledNotificationParam` (camelCase `requestId`).
                self.notify(
                    "notifications/cancelled",
                    Some(serde_json::json!({
                        "requestId": id,
                        "reason": "request timeout",
                    })),
                );
                Err(format!(
                    "Timed out after {timeout_ms}ms waiting for \"{method}\" response"
                ))
            }
        }
    }

    fn notify(&self, method: &str, params: Option<Value>) {
        let mut map = Map::new();
        map.insert("jsonrpc".to_string(), Value::String("2.0".to_string()));
        map.insert("method".to_string(), Value::String(method.to_string()));
        if let Some(p) = params {
            map.insert("params".to_string(), p);
        }
        self.write_value(&Value::Object(map));
    }

    fn send_result(&self, id: &Value, result: Value) {
        let mut map = Map::new();
        map.insert("jsonrpc".to_string(), Value::String("2.0".to_string()));
        map.insert("id".to_string(), id.clone());
        map.insert("result".to_string(), result);
        self.write_value(&Value::Object(map));
    }

    fn send_error(&self, id: &Value, code: i64, message: &str, data: Option<Value>) {
        let mut error = Map::new();
        error.insert("code".to_string(), Value::Number(code.into()));
        error.insert("message".to_string(), Value::String(message.to_string()));
        if let Some(d) = data {
            // TS JSON.stringify omits the key entirely when data is undefined.
            error.insert("data".to_string(), d);
        }
        let mut map = Map::new();
        map.insert("jsonrpc".to_string(), Value::String("2.0".to_string()));
        map.insert("id".to_string(), id.clone());
        map.insert("error".to_string(), Value::Object(error));
        self.write_value(&Value::Object(map));
    }

    /// Fail any in-flight server-initiated requests so their awaiters don't
    /// hang. Called from `stop()`/close paths.
    fn reject_pending(&self, reason: &str) {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        for (_, tx) in pending.drain() {
            // Send can only fail if the requester already gave up (timed out
            // and dropped its receiver) — nothing left to notify.
            tx.send(Err(reason.to_string())).ok();
        }
    }

    /// Handle an incoming line of JSON. Both transports feed lines here.
    /// `dispatch` queues a validated request/notification for the serial
    /// handler thread.
    fn handle_line(&self, line: &str, dispatch: &dyn Fn(IncomingMessage)) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return;
        }

        let parsed: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => {
                self.send_error(
                    &Value::Null,
                    ErrorCodes::PARSE_ERROR,
                    "Parse error: invalid JSON",
                    None,
                );
                return;
            }
        };

        // Response to a server-initiated request (has id + result/error, no
        // method). Route it to the awaiting requester instead of the message
        // handler — these used to be dropped as "Invalid Request" because they
        // carry no method.
        if let Some(obj) = parsed.as_object() {
            let is_response = obj.get("jsonrpc").and_then(|v| v.as_str()) == Some("2.0")
                && !matches!(obj.get("method"), Some(Value::String(_)))
                && obj.contains_key("id")
                && (obj.contains_key("result") || obj.contains_key("error"));
            if is_response {
                self.handle_response(obj);
                return;
            }
        }

        // Validate basic JSON-RPC structure.
        let valid = parsed
            .as_object()
            .map(|obj| {
                obj.get("jsonrpc").and_then(|v| v.as_str()) == Some("2.0")
                    && matches!(obj.get("method"), Some(Value::String(_)))
            })
            .unwrap_or(false);
        if !valid {
            self.send_error(
                &Value::Null,
                ErrorCodes::INVALID_REQUEST,
                "Invalid Request: not a valid JSON-RPC 2.0 message",
                None,
            );
            return;
        }

        let obj = parsed.as_object().unwrap();
        let message = IncomingMessage {
            id: obj.get("id").cloned(),
            method: obj
                .get("method")
                .and_then(|m| m.as_str())
                .unwrap_or_default()
                .to_string(),
            params: obj.get("params").cloned(),
        };

        // Notifications may be intercepted on the reader thread so they are
        // observed even while a handler blocks the serial dispatcher (rmcp
        // handles `notifications/cancelled` the same way, ahead of dispatch).
        if !message.is_request() {
            let interceptor = self
                .notification_interceptor
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(intercept) = interceptor.as_ref() {
                if intercept(&message) {
                    return;
                }
            }
        }

        dispatch(message);
    }

    /// Resolve (or reject) the pending server-initiated request matching this
    /// response's id. Unknown ids are ignored — the client may echo something
    /// we never sent, or a request may have already timed out.
    fn handle_response(&self, msg: &Map<String, Value>) {
        // Our generated ids are always `<prefix>-<n>` strings.
        let key = match msg.get("id") {
            Some(Value::String(s)) => s.clone(),
            _ => return,
        };
        let tx = match self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key)
        {
            Some(tx) => tx,
            None => return,
        };
        match msg.get("error") {
            Some(err) if js_truthy(err) => {
                let message = err
                    .get("message")
                    .and_then(|m| m.as_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or("Request failed");
                // Failure means the requester timed out and dropped its
                // receiver between our `remove` and this send — benign.
                tx.send(Err(message.to_string())).ok();
            }
            _ => {
                tx.send(Ok(msg.get("result").cloned().unwrap_or(Value::Null)))
                    .ok();
            }
        }
    }
}

/// Queue a validated request/notification for the serial dispatcher without
/// ever blocking the reader thread. On overflow (client pipelining faster
/// than the handler drains): requests get an immediate error response so the
/// client isn't left waiting, notifications are dropped (fire-and-forget;
/// `notifications/cancelled` is intercepted on the reader thread before
/// dispatch, so it is never lost here).
fn queue_or_reject(
    core: &LineRpcCore,
    tx: &crossbeam_channel::Sender<IncomingMessage>,
    message: IncomingMessage,
) {
    match tx.try_send(message) {
        Ok(()) => {}
        Err(crossbeam_channel::TrySendError::Full(message)) => {
            if let Some(id) = message.id {
                core.send_error(
                    &id,
                    ErrorCodes::INTERNAL_ERROR,
                    "Server overloaded: dispatch queue is full",
                    None,
                );
            }
        }
        // Dispatcher gone — connection is shutting down; nothing to do.
        Err(crossbeam_channel::TrySendError::Disconnected(_)) => {}
    }
}

/// Spawn a named transport thread, surfacing the (rare, fatal-for-this-
/// connection) spawn failure on stderr instead of swallowing it — a transport
/// with no reader/dispatcher thread is silently dead otherwise.
fn spawn_transport_thread<F: FnOnce() + Send + 'static>(name: &str, f: F) {
    // 16 MiB stack: these dispatch threads run the MCP tool handlers, which
    // include recursive graph traversals (impact, type hierarchy, file-tree
    // render). Those are individually stacker-guarded, but a roomy base stack
    // keeps segment switches off the hot path and matches the parse pool.
    if let Err(err) = std::thread::Builder::new()
        .name(name.to_string())
        .stack_size(16 * 1024 * 1024)
        .spawn(f)
    {
        eprintln!("[CodeGraph MCP] failed to spawn {name} thread: {err}");
    }
}

/// Run one dispatched message through the handler, reporting request-handler
/// failures back to the client (TS `handleLine`'s catch).
fn run_handler(core: &LineRpcCore, handler: &mut MessageHandler, message: IncomingMessage) {
    let id = message.id.clone();
    if let Err(err) = handler(message) {
        if let Some(id) = id {
            core.send_error(
                &id,
                ErrorCodes::INTERNAL_ERROR,
                &format!("Internal error: {err}"),
                None,
            );
        }
    }
}

/// Closed-flag + condvar pair backing `wait_until_closed`.
#[derive(Default)]
struct CloseLatch {
    closed: Mutex<bool>,
    cv: Condvar,
}

impl CloseLatch {
    fn signal(&self) {
        let mut closed = self.closed.lock().unwrap_or_else(|e| e.into_inner());
        *closed = true;
        self.cv.notify_all();
    }

    fn wait(&self) {
        let mut closed = self.closed.lock().unwrap_or_else(|e| e.into_inner());
        while !*closed {
            closed = self.cv.wait(closed).unwrap_or_else(|e| e.into_inner());
        }
    }
}

// =============================================================================
// StdioTransport
// =============================================================================

/// Options for [`StdioTransport`].
pub struct StdioTransportOptions {
    /// If true, the transport calls `process::exit(0)` when stdin closes. Set
    /// to `false` in shared-daemon mode where the stdio "session" is just
    /// *one* of many clients — losing it shouldn't drag the daemon down. The
    /// default (true) matches the original single-process behavior callers
    /// rely on.
    pub exit_on_close: bool,
    /// Optional callback fired when the stdin stream closes. The daemon uses
    /// this to decrement its connected-clients refcount.
    pub on_close: Option<Box<dyn FnOnce() + Send>>,
}

impl Default for StdioTransportOptions {
    fn default() -> Self {
        StdioTransportOptions {
            exit_on_close: true,
            on_close: None,
        }
    }
}

/// Stdio Transport for MCP.
///
/// Reads JSON-RPC messages from stdin and writes responses to stdout. Used by
/// the direct (single-process) MCP server path, where the MCP host launches
/// one server per session and talks to it over the child's stdio.
pub struct StdioTransport {
    core: Arc<LineRpcCore>,
    exit_on_close: bool,
    on_close: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    latch: Arc<CloseLatch>,
    started: AtomicBool,
}

impl StdioTransport {
    pub fn new(opts: StdioTransportOptions) -> Self {
        let write_line: Box<dyn Fn(&str) -> std::io::Result<()> + Send + Sync> =
            Box::new(|line: &str| {
                let stdout = std::io::stdout();
                let mut out = stdout.lock();
                out.write_all(line.as_bytes())?;
                out.write_all(b"\n")?;
                out.flush()
            });
        StdioTransport {
            core: Arc::new(LineRpcCore::new("cg-srv".to_string(), write_line)),
            exit_on_close: opts.exit_on_close,
            on_close: Mutex::new(opts.on_close),
            latch: Arc::new(CloseLatch::default()),
            started: AtomicBool::new(false),
        }
    }

    /// Block until the transport closes (stdin EOF or `stop()`). With the
    /// default `exit_on_close: true` the process exits before this returns.
    pub fn wait_until_closed(&self) {
        self.latch.wait();
    }
}

impl JsonRpcTransport for StdioTransport {
    fn start(&self, handler: MessageHandler) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }

        // Serial handler dispatch (mirrors the single JS event loop). Bounded:
        // see MAX_DISPATCH_QUEUE_MESSAGES.
        let (tx, rx) = crossbeam_channel::bounded::<IncomingMessage>(MAX_DISPATCH_QUEUE_MESSAGES);
        {
            let core = Arc::clone(&self.core);
            let mut handler = handler;
            spawn_transport_thread("cg-mcp-stdio-dispatch", move || {
                for message in rx {
                    run_handler(&core, &mut handler, message);
                }
            });
        }

        // Reader thread: stdin lines → core (responses routed inline,
        // requests/notifications queued for the dispatcher).
        let core = Arc::clone(&self.core);
        let latch = Arc::clone(&self.latch);
        let exit_on_close = self.exit_on_close;
        let on_close = self
            .on_close
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        spawn_transport_thread("cg-mcp-stdio-reader", move || {
            let stdin = std::io::stdin();
            let reader = stdin.lock();
            for line in reader.lines() {
                if core.stopped.load(Ordering::SeqCst) {
                    break;
                }
                match line {
                    Ok(line) => core.handle_line(&line, &|msg| queue_or_reject(&core, &tx, msg)),
                    Err(_) => break,
                }
            }
            // stdin closed (or read error): mirror readline's 'close'.
            core.reject_pending("Transport stopped");
            if let Some(cb) = on_close {
                cb();
            }
            latch.signal();
            if exit_on_close {
                std::process::exit(0);
            }
        });
    }

    fn stop(&self) {
        if self.core.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        self.core.reject_pending("Transport stopped");
        self.latch.signal();
    }

    fn set_notification_interceptor(&self, interceptor: NotificationInterceptor) {
        self.core.set_notification_interceptor(interceptor);
    }

    fn send(&self, response: &Value) {
        self.core.write_value(response);
    }

    fn notify(&self, method: &str, params: Option<Value>) {
        self.core.notify(method, params);
    }

    fn request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout_ms: Option<u64>,
    ) -> std::result::Result<Value, String> {
        self.core.request(method, params, timeout_ms)
    }

    fn send_result(&self, id: &Value, result: Value) {
        self.core.send_result(id, result);
    }

    fn send_error(&self, id: &Value, code: i64, message: &str, data: Option<Value>) {
        self.core.send_error(id, code, message, data);
    }
}

// =============================================================================
// SocketTransport (unix only — named pipes are not implemented; see
// notes/mcp-daemon.md "Platform gates")
// =============================================================================

#[cfg(unix)]
pub use socket_transport::SocketTransport;

#[cfg(unix)]
mod socket_transport {
    use std::io::BufReader;
    use std::os::unix::net::UnixStream;

    use super::{
        Arc,
        AtomicBool,
        BufRead,
        CloseLatch,
        IncomingMessage,
        JsonRpcTransport,
        LineRpcCore,
        MAX_DISPATCH_QUEUE_MESSAGES,
        MessageHandler,
        Mutex,
        NotificationInterceptor,
        Ordering,
        Value,
        Write,
        queue_or_reject,
        run_handler,
        spawn_transport_thread,
    };

    /// Socket Transport for MCP daemon sessions.
    ///
    /// Wraps a single Unix-domain socket. One instance per connected MCP
    /// client. Unlike [`StdioTransport`], `stop()` and stream-close *don't*
    /// call `process::exit` — a daemon-side session ending must not bring
    /// down the whole daemon.
    pub struct SocketTransport {
        core: Arc<LineRpcCore>,
        socket: Mutex<UnixStream>,
        close_handlers: Arc<Mutex<Vec<Box<dyn FnOnce() + Send>>>>,
        latch: Arc<CloseLatch>,
        started: AtomicBool,
    }

    impl SocketTransport {
        pub fn new(socket: UnixStream) -> Self {
            Self::with_prefix(socket, "cg-sock")
        }

        pub fn with_prefix(socket: UnixStream, prefix: &str) -> Self {
            let writer = socket.try_clone().ok().map(Mutex::new);
            let write_line: Box<dyn Fn(&str) -> std::io::Result<()> + Send + Sync> =
                Box::new(move |line: &str| {
                    let Some(w) = &writer else {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::NotConnected,
                            "socket writer unavailable",
                        ));
                    };
                    let mut stream = w.lock().unwrap_or_else(|e| e.into_inner());
                    stream.write_all(line.as_bytes())?;
                    stream.write_all(b"\n")?;
                    stream.flush()
                });
            SocketTransport {
                core: Arc::new(LineRpcCore::new(prefix.to_string(), write_line)),
                socket: Mutex::new(socket),
                close_handlers: Arc::new(Mutex::new(Vec::new())),
                latch: Arc::new(CloseLatch::default()),
                started: AtomicBool::new(false),
            }
        }

        /// Register a callback fired exactly once when the socket closes
        /// (from either side). Used by the daemon to decrement its
        /// connected-clients refcount.
        pub fn on_close(&self, handler: Box<dyn FnOnce() + Send>) {
            self.close_handlers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(handler);
        }

        /// Block until the socket closes (from either side) or `stop()` runs.
        /// Lets the daemon's per-connection thread run a session to
        /// completion.
        pub fn wait_until_closed(&self) {
            self.latch.wait();
        }

        /// Reader-thread body: socket lines → core (responses routed inline,
        /// requests/notifications queued for the dispatcher). Returns when the
        /// peer disconnects, a read errors, or `stop()` flips the flag.
        fn read_loop(
            core: &LineRpcCore,
            stream: UnixStream,
            tx: &crossbeam_channel::Sender<IncomingMessage>,
        ) {
            let reader = BufReader::new(stream);
            for line in reader.lines() {
                if core.stopped.load(Ordering::SeqCst) {
                    return;
                }
                match line {
                    Ok(line) => core.handle_line(&line, &|msg| queue_or_reject(core, tx, msg)),
                    Err(err) => {
                        // Don't crash the daemon over a broken pipe; just
                        // shut this connection.
                        eprintln!("[CodeGraph daemon] socket error: {err}");
                        return;
                    }
                }
            }
        }

        fn handle_socket_close(
            core: &LineRpcCore,
            close_handlers: &Mutex<Vec<Box<dyn FnOnce() + Send>>>,
            latch: &CloseLatch,
        ) {
            if core.stopped.swap(true, Ordering::SeqCst) {
                return;
            }
            core.reject_pending("Socket closed");
            let handlers: Vec<_> = close_handlers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .drain(..)
                .collect();
            for h in handlers {
                // Never let a close-handler take the daemon down.
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(h)).ok();
            }
            latch.signal();
        }
    }

    impl JsonRpcTransport for SocketTransport {
        fn start(&self, handler: MessageHandler) {
            if self.started.swap(true, Ordering::SeqCst) {
                return;
            }

            // Bounded: see MAX_DISPATCH_QUEUE_MESSAGES.
            let (tx, rx) =
                crossbeam_channel::bounded::<IncomingMessage>(MAX_DISPATCH_QUEUE_MESSAGES);
            {
                let core = Arc::clone(&self.core);
                let mut handler = handler;
                spawn_transport_thread("cg-mcp-sock-dispatch", move || {
                    for message in rx {
                        run_handler(&core, &mut handler, message);
                    }
                });
            }

            let read_half = self
                .socket
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .try_clone();
            let core = Arc::clone(&self.core);
            let close_handlers = Arc::clone(&self.close_handlers);
            let latch = Arc::clone(&self.latch);
            spawn_transport_thread("cg-mcp-sock-reader", move || {
                match read_half {
                    Ok(stream) => SocketTransport::read_loop(&core, stream, &tx),
                    Err(err) => {
                        eprintln!("[CodeGraph daemon] socket error: {err}");
                    }
                }
                SocketTransport::handle_socket_close(&core, &close_handlers, &latch);
            });
        }

        fn stop(&self) {
            if self.core.stopped.swap(true, Ordering::SeqCst) {
                return;
            }
            self.core.reject_pending("Transport stopped");
            let stream = self.socket.lock().unwrap_or_else(|e| e.into_inner());
            // Shutdown fails only if the peer already closed — benign here.
            stream.shutdown(std::net::Shutdown::Both).ok();
            self.latch.signal();
        }

        fn set_notification_interceptor(&self, interceptor: NotificationInterceptor) {
            self.core.set_notification_interceptor(interceptor);
        }

        fn send(&self, response: &Value) {
            self.core.write_value(response);
        }

        fn notify(&self, method: &str, params: Option<Value>) {
            self.core.notify(method, params);
        }

        fn request(
            &self,
            method: &str,
            params: Option<Value>,
            timeout_ms: Option<u64>,
        ) -> std::result::Result<Value, String> {
            self.core.request(method, params, timeout_ms)
        }

        fn send_result(&self, id: &Value, result: Value) {
            self.core.send_result(id, result);
        }

        fn send_error(&self, id: &Value, code: i64, message: &str, data: Option<Value>) {
            self.core.send_error(id, code, message, data);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn collecting_core() -> (Arc<LineRpcCore>, Arc<Mutex<Vec<String>>>) {
        let out: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&out);
        let core = Arc::new(LineRpcCore::new(
            "cg-test".to_string(),
            Box::new(move |line: &str| {
                sink.lock().unwrap().push(line.to_string());
                Ok(())
            }),
        ));
        (core, out)
    }

    #[test]
    fn send_error_wire_shape_includes_null_id_and_omits_absent_data() {
        let (core, out) = collecting_core();
        core.send_error(
            &Value::Null,
            ErrorCodes::PARSE_ERROR,
            "Parse error: invalid JSON",
            None,
        );
        let lines = out.lock().unwrap();
        assert_eq!(
            lines[0],
            r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error: invalid JSON"}}"#
        );
    }

    #[test]
    fn send_result_wire_shape_matches_ts_key_order() {
        let (core, out) = collecting_core();
        core.send_result(&json!(0), json!({"ok": true}));
        let lines = out.lock().unwrap();
        assert_eq!(lines[0], r#"{"jsonrpc":"2.0","id":0,"result":{"ok":true}}"#);
    }

    #[test]
    fn invalid_json_line_produces_parse_error() {
        let (core, out) = collecting_core();
        core.handle_line("{nope", &|_msg| panic!("must not dispatch"));
        let lines = out.lock().unwrap();
        assert!(lines[0].contains("\"code\":-32700"));
        assert!(lines[0].contains("Parse error: invalid JSON"));
    }

    #[test]
    fn non_jsonrpc_object_produces_invalid_request() {
        let (core, out) = collecting_core();
        core.handle_line(r#"{"foo":1}"#, &|_msg| panic!("must not dispatch"));
        let lines = out.lock().unwrap();
        assert!(lines[0].contains("\"code\":-32600"));
        assert!(lines[0].contains("Invalid Request: not a valid JSON-RPC 2.0 message"));
    }

    #[test]
    fn empty_lines_are_ignored() {
        let (core, out) = collecting_core();
        core.handle_line("   ", &|_msg| panic!("must not dispatch"));
        assert!(out.lock().unwrap().is_empty());
    }

    #[test]
    fn request_resolves_when_the_matching_response_arrives() {
        let (core, _out) = collecting_core();
        let core2 = Arc::clone(&core);
        let t = std::thread::spawn(move || core2.request("roots/list", None, Some(2000)));
        // Wait for the request to be registered, then feed the response line.
        std::thread::sleep(Duration::from_millis(50));
        core.handle_line(
            r#"{"jsonrpc":"2.0","id":"cg-test-1","result":{"roots":[]}}"#,
            &|_msg| panic!("responses must not dispatch"),
        );
        let result = t.join().unwrap().unwrap();
        assert_eq!(result, json!({"roots": []}));
    }

    #[test]
    fn request_times_out_with_the_ts_error_string() {
        let (core, out) = collecting_core();
        let err = core.request("roots/list", None, Some(50)).unwrap_err();
        assert_eq!(
            err,
            "Timed out after 50ms waiting for \"roots/list\" response"
        );
        // The outgoing request envelope had the TS key order.
        let lines = out.lock().unwrap();
        assert_eq!(
            lines[0],
            r#"{"jsonrpc":"2.0","id":"cg-test-1","method":"roots/list"}"#
        );
        // EXCEEDS TS: the timeout path tells the client we abandoned the
        // request (rmcp REQUEST_TIMEOUT_REASON wire shape).
        assert_eq!(
            lines[1],
            r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"cg-test-1","reason":"request timeout"}}"#
        );
    }

    #[test]
    fn notification_interceptor_consumes_on_the_reader_path() {
        let (core, _out) = collecting_core();
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = Arc::clone(&seen);
        core.set_notification_interceptor(Box::new(move |msg| {
            if msg.method == "notifications/cancelled" {
                sink.lock().unwrap().push(msg.method.clone());
                return true; // consumed — must not dispatch
            }
            false
        }));

        // Consumed notification never reaches dispatch.
        core.handle_line(
            r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}"#,
            &|_msg| panic!("consumed notification must not dispatch"),
        );
        assert_eq!(seen.lock().unwrap().as_slice(), ["notifications/cancelled"]);

        // Unconsumed notifications and requests still dispatch normally.
        let dispatched = Arc::new(Mutex::new(Vec::<String>::new()));
        let dsink = Arc::clone(&dispatched);
        let dispatch = move |msg: IncomingMessage| {
            dsink.lock().unwrap().push(msg.method);
        };
        core.handle_line(r#"{"jsonrpc":"2.0","method":"initialized"}"#, &dispatch);
        // Requests bypass the interceptor entirely, even with a matching method.
        core.handle_line(
            r#"{"jsonrpc":"2.0","id":7,"method":"notifications/cancelled"}"#,
            &dispatch,
        );
        assert_eq!(
            dispatched.lock().unwrap().as_slice(),
            ["initialized", "notifications/cancelled"]
        );
    }

    #[test]
    fn error_response_rejects_with_its_message() {
        let (core, _out) = collecting_core();
        let core2 = Arc::clone(&core);
        let t = std::thread::spawn(move || core2.request("roots/list", None, Some(2000)));
        std::thread::sleep(Duration::from_millis(50));
        core.handle_line(
            r#"{"jsonrpc":"2.0","id":"cg-test-1","error":{"code":1,"message":"nope"}}"#,
            &|_msg| panic!("responses must not dispatch"),
        );
        assert_eq!(t.join().unwrap().unwrap_err(), "nope");
    }

    #[test]
    fn request_fails_fast_when_the_transport_write_fails() {
        let core = LineRpcCore::new(
            "cg-test".to_string(),
            Box::new(|_line: &str| Err(std::io::Error::other("broken pipe"))),
        );
        let started = std::time::Instant::now();
        let err = core.request("roots/list", None, Some(5000)).unwrap_err();
        // Must not have waited out the 5s timeout.
        assert!(started.elapsed() < Duration::from_millis(1000));
        assert_eq!(err, "Transport write failed sending \"roots/list\" request");
        // The pending entry must not leak.
        assert!(core.pending.lock().unwrap().is_empty());
    }

    #[test]
    fn dispatch_queue_overflow_fails_requests_and_drops_notifications() {
        let (core, out) = collecting_core();
        let (tx, _rx) = crossbeam_channel::bounded::<IncomingMessage>(1);
        let msg = |id: Option<Value>| IncomingMessage {
            id,
            method: "tools/call".to_string(),
            params: None,
        };
        // Fills the queue — no error written.
        queue_or_reject(&core, &tx, msg(Some(json!(1))));
        assert!(out.lock().unwrap().is_empty());
        // Overflowing request fails fast with an error response.
        queue_or_reject(&core, &tx, msg(Some(json!(2))));
        {
            let lines = out.lock().unwrap();
            assert_eq!(lines.len(), 1);
            assert!(lines[0].contains("\"id\":2"));
            assert!(lines[0].contains("\"code\":-32603"));
            assert!(lines[0].contains("Server overloaded"));
        }
        // Overflowing notification is dropped silently.
        queue_or_reject(&core, &tx, msg(None));
        assert_eq!(out.lock().unwrap().len(), 1);
        // Queue drains → sends succeed again.
        let drained = _rx.try_recv().unwrap();
        assert_eq!(drained.id, Some(json!(1)));
        queue_or_reject(&core, &tx, msg(Some(json!(3))));
        assert_eq!(out.lock().unwrap().len(), 1);
    }

    #[test]
    fn notification_dispatches_without_id_and_request_keeps_null_id() {
        let (core, _out) = collecting_core();
        let seen: Arc<Mutex<Vec<IncomingMessage>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let dispatch = move |msg: IncomingMessage| {
            sink.lock().unwrap().push(msg);
        };
        core.handle_line(r#"{"jsonrpc":"2.0","method":"initialized"}"#, &dispatch);
        core.handle_line(r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#, &dispatch);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(!seen[0].is_request());
        assert!(seen[1].is_request());
        assert_eq!(seen[1].id, Some(Value::Null));
    }
}

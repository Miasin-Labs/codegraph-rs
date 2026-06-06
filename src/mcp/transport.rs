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

/// Generic JSON-RPC transport interface — common surface for stdio and socket
/// carriers. Anything below the session layer (initialize, tool dispatch,
/// etc.) talks to this, not to a concrete transport struct.
pub trait JsonRpcTransport: Send + Sync {
    fn start(&self, handler: MessageHandler);
    fn stop(&self);
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

/// JS truthiness for the `if ('error' in msg && msg.error)` check.
fn js_truthy(v: &Value) -> bool {
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
    next_request_id: AtomicU64,
    pub(crate) stopped: AtomicBool,
    id_prefix: String,
    /// Writes one line (no trailing newline) to the underlying stream.
    write_line: Box<dyn Fn(&str) + Send + Sync>,
}

impl LineRpcCore {
    fn new(id_prefix: String, write_line: Box<dyn Fn(&str) + Send + Sync>) -> Self {
        LineRpcCore {
            pending: Mutex::new(HashMap::new()),
            next_request_id: AtomicU64::new(1),
            stopped: AtomicBool::new(false),
            id_prefix,
            write_line,
        }
    }

    fn write_value(&self, value: &Value) {
        if let Ok(line) = serde_json::to_string(value) {
            (self.write_line)(&line);
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
        self.write_value(&Value::Object(map));

        match rx.recv_timeout(Duration::from_millis(timeout_ms)) {
            Ok(result) => result,
            Err(_) => {
                self.pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id);
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
            let _ = tx.send(Err(reason.to_string()));
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
                let _ = tx.send(Err(message.to_string()));
            }
            _ => {
                let _ = tx.send(Ok(msg.get("result").cloned().unwrap_or(Value::Null)));
            }
        }
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
        let write_line: Box<dyn Fn(&str) + Send + Sync> = Box::new(|line: &str| {
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            let _ = out.write_all(line.as_bytes());
            let _ = out.write_all(b"\n");
            let _ = out.flush();
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

        // Serial handler dispatch (mirrors the single JS event loop).
        let (tx, rx) = crossbeam_channel::unbounded::<IncomingMessage>();
        {
            let core = Arc::clone(&self.core);
            let mut handler = handler;
            let _ = std::thread::Builder::new()
                .name("cg-mcp-stdio-dispatch".to_string())
                .spawn(move || {
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
        let _ = std::thread::Builder::new()
            .name("cg-mcp-stdio-reader".to_string())
            .spawn(move || {
                let stdin = std::io::stdin();
                let reader = stdin.lock();
                for line in reader.lines() {
                    if core.stopped.load(Ordering::SeqCst) {
                        break;
                    }
                    match line {
                        Ok(line) => core.handle_line(&line, &|msg| {
                            let _ = tx.send(msg);
                        }),
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

    use super::*;

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
            let write_line: Box<dyn Fn(&str) + Send + Sync> = Box::new(move |line: &str| {
                if let Some(w) = &writer {
                    let mut stream = w.lock().unwrap_or_else(|e| e.into_inner());
                    let _ = stream.write_all(line.as_bytes());
                    let _ = stream.write_all(b"\n");
                    let _ = stream.flush();
                }
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

        /// Write a one-shot line directly to the socket (no JSON-RPC framing
        /// applied — caller produces the line). The daemon uses this for the
        /// hello/handshake line that precedes the JSON-RPC stream.
        pub fn write_raw(&self, line: &str) {
            let mut stream = self.socket.lock().unwrap_or_else(|e| e.into_inner());
            if line.ends_with('\n') {
                let _ = stream.write_all(line.as_bytes());
            } else {
                let _ = stream.write_all(line.as_bytes());
                let _ = stream.write_all(b"\n");
            }
            let _ = stream.flush();
        }

        /// Block until the socket closes (from either side) or `stop()` runs.
        /// Lets the daemon's per-connection thread run a session to
        /// completion.
        pub fn wait_until_closed(&self) {
            self.latch.wait();
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
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(h));
            }
            latch.signal();
        }
    }

    impl JsonRpcTransport for SocketTransport {
        fn start(&self, handler: MessageHandler) {
            if self.started.swap(true, Ordering::SeqCst) {
                return;
            }

            let (tx, rx) = crossbeam_channel::unbounded::<IncomingMessage>();
            {
                let core = Arc::clone(&self.core);
                let mut handler = handler;
                let _ = std::thread::Builder::new()
                    .name("cg-mcp-sock-dispatch".to_string())
                    .spawn(move || {
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
            let _ = std::thread::Builder::new()
                .name("cg-mcp-sock-reader".to_string())
                .spawn(move || {
                    match read_half {
                        Ok(stream) => {
                            let reader = BufReader::new(stream);
                            for line in reader.lines() {
                                if core.stopped.load(Ordering::SeqCst) {
                                    break;
                                }
                                match line {
                                    Ok(line) => core.handle_line(&line, &|msg| {
                                        let _ = tx.send(msg);
                                    }),
                                    Err(err) => {
                                        // Don't crash the daemon over a broken
                                        // pipe; just shut this connection.
                                        eprintln!("[CodeGraph daemon] socket error: {err}");
                                        break;
                                    }
                                }
                            }
                        }
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
            let _ = stream.shutdown(std::net::Shutdown::Both);
            self.latch.signal();
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

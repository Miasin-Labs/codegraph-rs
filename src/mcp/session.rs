//! MCP per-connection session — speaks the JSON-RPC protocol (initialize,
//! tools/list, tools/call) over a single [`JsonRpcTransport`]. It owns
//! per-client state only (which protocol version the client asked for,
//! whether it advertised `roots`, the one-shot roots/list latch); the
//! heavyweight resources (CodeGraph, watcher, ToolHandler) live in the shared
//! engine (driven through [`EngineHandle`]) so daemon mode can collapse N
//! inotify sets / DB handles to one.
//!
//! The state-machine itself mirrors what `MCPServer` used to do inline before
//! issue #411 split it out — the same regression tests in
//! `__tests__/mcp-initialize.test.ts` still drive this code path (ported in
//! `rust/tests/mcp_server_test.rs`).
//!
//! Port of `src/mcp/session.ts`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crossbeam_channel::Receiver;
use serde_json::{Map, Value, json};

use crate::mcp::engine::{EngineHandle, LogSubscription, logging_level_rank};
use crate::mcp::server_instructions::SERVER_INSTRUCTIONS;
use crate::mcp::tools::{ProgressEmitter, tools};
use crate::mcp::transport::{ErrorCodes, IncomingMessage, JsonRpcTransport, js_truthy};
use crate::mcp::version::CODEGRAPH_PACKAGE_VERSION;

/// MCP Server Info — kept on the session because some clients log it. The
/// version tracks the real package version (was a hard-coded '0.1.0').
///
/// Exported so the proxy can answer `initialize` locally with the IDENTICAL
/// payload the daemon would send — no drift between the two handshake paths.
/// (TS `SERVER_INFO`; key order `name`, `version`.)
pub fn server_info() -> Value {
    json!({
        "name": "codegraph",
        "version": CODEGRAPH_PACKAGE_VERSION,
    })
}

/// Server capabilities advertised in the `initialize` result. Exported so the
/// proxy's locally-answered handshake stays byte-identical to the daemon's.
///
/// EXCEEDS TS (which advertises bare `{"tools": {}}`): adds
/// `tools.listChanged` (the list IS dynamic — tiny-repo gating and the
/// explore-budget suffix appear once a project opens) and `logging`
/// (`logging/setLevel` + mirrored `[CodeGraph MCP]` diagnostics as
/// `notifications/message`). Mirrors rmcp `ToolsCapability { list_changed }`
/// and `ServerCapabilities` (logging before tools, camelCase).
pub fn server_capabilities() -> Value {
    json!({
        "logging": {},
        "tools": { "listChanged": true },
    })
}

/// MCP protocol versions this server knows how to speak, oldest to newest.
const PROTOCOL_VERSIONS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-06-18"];

/// MCP Protocol Version (latest the server claims).
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// TS `negotiatedProtocolVersion(clientVersion)`: a known client version is
/// echoed back; anything else (unknown / non-string / absent) gets the latest
/// version the server speaks.
pub fn negotiated_protocol_version(client_version: Option<&Value>) -> String {
    let client = match client_version {
        Some(Value::String(s)) => s.as_str(),
        _ => return PROTOCOL_VERSION.to_string(),
    };
    match PROTOCOL_VERSIONS.iter().position(|v| *v == client) {
        Some(idx) => PROTOCOL_VERSIONS[idx.min(PROTOCOL_VERSIONS.len() - 1)].to_string(),
        None => PROTOCOL_VERSION.to_string(),
    }
}

/// How long to wait for the client's `roots/list` response before giving up
/// and falling back to the process cwd.
const ROOTS_LIST_TIMEOUT_MS: u64 = 5000;

/// Lenient `decodeURIComponent`: decodes valid `%XX` escapes (UTF-8 byte
/// sequences); malformed escapes are left verbatim (the TS version throws and
/// the caller falls back — for the URIs MCP clients actually send the two
/// behaviors coincide).
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            if let Some(hex) = bytes.get(i + 1..i + 3) {
                if let Ok(byte) = u8::from_str_radix(std::str::from_utf8(hex).unwrap_or("!"), 16) {
                    out.push(byte);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Extract the WHATWG-URL `pathname` from a `scheme://[host]/path` URI.
/// Returns `None` when the input doesn't parse as a URL (TS `new URL(uri)`
/// throwing).
fn url_pathname(uri: &str) -> Option<String> {
    let scheme_end = uri.find("://")?;
    let scheme = &uri[..scheme_end];
    if scheme.is_empty()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
        || !scheme
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
    {
        return None;
    }
    let rest = &uri[scheme_end + 3..];
    let pathname = match rest.find('/') {
        Some(idx) => &rest[idx..],
        // `file://host` with no path — URL.pathname is "/".
        None => "/",
    };
    // Strip query/fragment.
    let pathname = pathname.split(['?', '#']).next().unwrap_or(pathname);
    Some(pathname.to_string())
}

/// Convert a file:// URI to a filesystem path. Handles URL encoding and
/// Windows drive letter paths. (TS `fileUriToPath`.)
pub(crate) fn file_uri_to_path(uri: &str) -> String {
    if let Some(pathname) = url_pathname(uri) {
        #[allow(unused_mut)]
        let mut file_path = percent_decode(&pathname);
        #[cfg(windows)]
        {
            // /C:/foo → C:/foo
            let b = file_path.as_bytes();
            if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
                file_path = file_path[1..].to_string();
            }
        }
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        return crate::utils::lexical_resolve(&cwd, &file_path)
            .to_string_lossy()
            .to_string();
    }
    // TS fallback: uri.replace(/^file:\/\/\/?/, '')
    if let Some(rest) = uri.strip_prefix("file://") {
        return rest.strip_prefix('/').unwrap_or(rest).to_string();
    }
    uri.to_string()
}

/// First usable filesystem path from a `roots/list` result, or None.
fn first_root_path(result: &Value) -> Option<String> {
    let roots = result.get("roots")?.as_array()?;
    let first = roots.first()?;
    let uri = first.get("uri")?.as_str()?;
    Some(file_uri_to_path(uri))
}

fn process_cwd() -> String {
    std::env::current_dir()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_else(|_| String::from("/"))
}

/// Options for [`MCPSession`].
#[derive(Default)]
pub struct MCPSessionOptions {
    /// Explicit project path from the `--path` CLI flag. When set, the
    /// session will not bother asking the client for `roots/list` — we
    /// already know where the project lives.
    pub explicit_project_path: Option<String>,
}

struct SessionState {
    client_supports_roots: bool,
    roots_attempted: bool,
    /// EXCEEDS TS: set by `notifications/roots/list_changed` while no project
    /// is resolved — re-arms the one-shot roots/list latch so the next
    /// `retry_init_if_needed` re-asks the client (rmcp
    /// `on_roots_list_changed`).
    roots_refresh_requested: bool,
    /// In-flight background init kicked off from `handle_initialize`
    /// (TS `resolvePromise`).
    resolve_pending: Option<Receiver<()>>,
    /// EXCEEDS TS: serialized tool list last served to this client via
    /// `tools/list` — when a later project open changes the gated/budgeted
    /// list, we emit `notifications/tools/list_changed`.
    last_listed_tools: Option<String>,
}

struct SessionInner {
    transport: Arc<dyn JsonRpcTransport>,
    engine: EngineHandle,
    explicit_project_path: Option<String>,
    state: Mutex<SessionState>,
    /// EXCEEDS TS: cancel flags for in-flight `tools/call` requests, keyed by
    /// the serialized request id (rmcp `local_ct_pool` keyed by `RequestId`).
    /// `initialize` is never tracked — it is never cancellable.
    in_flight: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// EXCEEDS TS: this session's `logging` subscription (min level set via
    /// `logging/setLevel`; engine diagnostics fan out through it).
    log_subscription: Arc<LogSubscription>,
}

/// One MCP client's view of the server. Created fresh per stdio launch
/// (direct mode) or per socket connection (daemon mode).
pub struct MCPSession {
    inner: Arc<SessionInner>,
}

impl MCPSession {
    pub fn new(
        transport: Arc<dyn JsonRpcTransport>,
        engine: EngineHandle,
        opts: MCPSessionOptions,
    ) -> MCPSession {
        // EXCEEDS TS: subscribe this session to the engine's mirrored
        // diagnostics (the `logging` capability). The subscription holds the
        // transport weakly, so a closed session is pruned automatically.
        let log_subscription = LogSubscription::new(&transport);
        engine.register_log_subscriber(Arc::clone(&log_subscription));
        MCPSession {
            inner: Arc::new(SessionInner {
                transport,
                engine,
                explicit_project_path: opts.explicit_project_path,
                state: Mutex::new(SessionState {
                    client_supports_roots: false,
                    roots_attempted: false,
                    roots_refresh_requested: false,
                    resolve_pending: None,
                    last_listed_tools: None,
                }),
                in_flight: Mutex::new(HashMap::new()),
                log_subscription,
            }),
        }
    }

    /// Start handling messages from the transport. Returns immediately — the
    /// session lives for as long as the transport is open.
    pub fn start(&self) {
        // EXCEEDS TS: intercept `notifications/cancelled` on the reader
        // thread — the serial dispatcher may be blocked inside a long
        // `tools/call`, and a cancellation queued behind it would always
        // arrive too late (rmcp likewise intercepts ahead of dispatch).
        // Weak: the interceptor must not keep the session alive via the
        // transport it is installed on.
        let weak = Arc::downgrade(&self.inner);
        self.inner
            .transport
            .set_notification_interceptor(Box::new(move |message| {
                if message.method != "notifications/cancelled" {
                    return false;
                }
                if let Some(inner) = weak.upgrade() {
                    inner.handle_cancelled_notification(message.params.as_ref());
                }
                true
            }));
        let inner = Arc::clone(&self.inner);
        self.inner.transport.start(Box::new(move |message| {
            inner.handle_message(message);
            Ok(())
        }));
    }

    /// Tear down the session. Does NOT touch the engine (the engine may serve
    /// other sessions) or exit the process (the daemon decides when to exit).
    pub fn stop(&self) {
        self.inner.transport.stop();
    }

    /// Underlying transport — exposed for daemon-side close hooks.
    pub fn get_transport(&self) -> Arc<dyn JsonRpcTransport> {
        Arc::clone(&self.inner.transport)
    }
}

impl SessionInner {
    fn handle_message(&self, message: IncomingMessage) {
        let is_request = message.is_request();
        match message.method.as_str() {
            "initialize" => {
                if is_request {
                    self.handle_initialize(&message);
                }
            }
            // EXCEEDS TS (session.ts:140 matches only the bare legacy
            // string): the spec method name is `notifications/initialized`
            // (rmcp `InitializedNotificationMethod`); both spellings hit the
            // same no-op arm so the handshake hook is real, not an accident
            // of the default arm tolerating unknown notifications.
            "initialized" | "notifications/initialized" => {
                // Notification that client has finished initialization — no
                // action needed.
            }
            "tools/list" => {
                if is_request {
                    self.handle_tools_list(&message);
                }
            }
            "tools/call" => {
                if is_request {
                    self.handle_tools_call(&message);
                }
            }
            "logging/setLevel" => {
                // EXCEEDS TS: `logging` capability (advertised in the
                // initialize result) — store the session's minimum level and
                // ack with an empty result (rmcp `SetLevelRequestParams`).
                if is_request {
                    self.handle_set_level(&message);
                }
            }
            "notifications/roots/list_changed" => {
                // EXCEEDS TS: mirrors rmcp `on_roots_list_changed` — a host
                // that adds a workspace folder after connect re-arms the
                // one-shot roots/list latch (only while no project resolved
                // and no explicit --path pinned the project).
                if !self.engine.has_default_code_graph() && self.explicit_project_path.is_none() {
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.roots_attempted = false;
                    state.roots_refresh_requested = true;
                }
            }
            "ping" => {
                if let Some(id) = &message.id {
                    self.transport.send_result(id, json!({}));
                }
            }
            "resources/list" => {
                // We expose no MCP resources, but some clients (opencode,
                // Codex) probe for them on connect; reply with an empty list
                // instead of a MethodNotFound error that surfaces as a scary
                // `-32601` log line. (#621)
                if let Some(id) = &message.id {
                    self.transport.send_result(id, json!({ "resources": [] }));
                }
            }
            "resources/templates/list" => {
                if let Some(id) = &message.id {
                    self.transport
                        .send_result(id, json!({ "resourceTemplates": [] }));
                }
            }
            "prompts/list" => {
                // Likewise — no prompts exposed, but answer the probe
                // cleanly. (#621)
                if let Some(id) = &message.id {
                    self.transport.send_result(id, json!({ "prompts": [] }));
                }
            }
            method => {
                if let Some(id) = &message.id {
                    self.transport.send_error(
                        id,
                        ErrorCodes::METHOD_NOT_FOUND,
                        &format!("Method not found: {method}"),
                        None,
                    );
                }
            }
        }
    }

    fn handle_initialize(&self, request: &IncomingMessage) {
        let params = request.params.as_ref();
        let get = |key: &str| params.and_then(|p| p.get(key));

        let client_supports_roots = get("capabilities")
            .and_then(|c| c.get("roots"))
            .map(js_truthy)
            .unwrap_or(false);
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.client_supports_roots = client_supports_roots;
        }

        // Explicit project signal, strongest first: client-provided rootUri /
        // workspaceFolders (LSP-style), else the --path the server was
        // launched with. cwd is NOT used here — we defer it so a roots/list
        // answer can win over it. See issue #196.
        let mut explicit_path: Option<String> = None;
        if let Some(uri) = get("rootUri")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            explicit_path = Some(file_uri_to_path(uri));
        } else if let Some(uri) = get("workspaceFolders")
            .and_then(|w| w.as_array())
            .and_then(|arr| arr.first())
            .and_then(|f| f.get("uri"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            explicit_path = Some(file_uri_to_path(uri));
        } else if let Some(p) = &self.explicit_project_path {
            explicit_path = Some(p.clone());
        }

        // Respond to the handshake BEFORE doing any heavy init — see issue
        // #172. Result key order matches TS: protocolVersion, capabilities,
        // serverInfo, instructions.
        let mut result = Map::new();
        result.insert(
            "protocolVersion".to_string(),
            Value::String(negotiated_protocol_version(get("protocolVersion"))),
        );
        result.insert("capabilities".to_string(), server_capabilities());
        result.insert("serverInfo".to_string(), server_info());
        result.insert(
            "instructions".to_string(),
            Value::String(SERVER_INSTRUCTIONS.to_string()),
        );
        if let Some(id) = &request.id {
            self.transport.send_result(id, Value::Object(result));
        }

        if let Some(path) = explicit_path {
            // Kick off engine init in the background. If another session in
            // the same daemon already opened the project,
            // `ensure_initialized` is a ~free no-op — N concurrent clients
            // pay exactly one open.
            let rx = self.engine.ensure_initialized_async(&path);
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.resolve_pending = Some(rx);
        }
    }

    fn handle_tools_list(&self, request: &IncomingMessage) {
        self.retry_init_if_needed();
        let tools = self.engine.get_tools();
        let tools_value = serde_json::to_value(&tools).unwrap_or(Value::Array(vec![]));
        if let Some(id) = &request.id {
            self.transport
                .send_result(id, json!({ "tools": tools_value }));
        }
        // Remember what the client saw so a later project open that changes
        // the gated/budgeted list triggers `notifications/tools/list_changed`
        // (EXCEEDS TS).
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.last_listed_tools = serde_json::to_string(&tools).ok();
    }

    fn handle_tools_call(&self, request: &IncomingMessage) {
        let id = match &request.id {
            Some(id) => id,
            None => return,
        };
        let params = request.params.as_ref();
        let name_value = params.and_then(|p| p.get("name"));
        let tool_name = match name_value {
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            // TS: `if (!params || !params.name)` — falsy name → missing.
            Some(v) if js_truthy(v) => {
                // A truthy non-string name passes the TS guard and fails the
                // lookup below ("Unknown tool: <stringified>").
                stringify_js(v)
            }
            _ => {
                self.transport.send_error(
                    id,
                    ErrorCodes::INVALID_PARAMS,
                    "Missing tool name",
                    None,
                );
                return;
            }
        };

        let tool_args = params
            .and_then(|p| p.get("arguments"))
            .filter(|v| js_truthy(v))
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));

        if !tools().iter().any(|t| t.name == tool_name) {
            self.transport.send_error(
                id,
                ErrorCodes::INVALID_PARAMS,
                &format!("Unknown tool: {tool_name}"),
                None,
            );
            return;
        }

        self.retry_init_if_needed();

        // EXCEEDS TS: honor `_meta.progressToken` (string or integer, per
        // rmcp `Meta::get_progress_token` — floats ignored). When present,
        // the engine's long stages (catch-up sync) emit
        // `notifications/progress` through this emitter; absent token ⇒ no
        // emitter ⇒ nothing is ever sent unsolicited.
        let progress_token = params
            .and_then(|p| p.get("_meta"))
            .and_then(|m| m.get("progressToken"))
            .and_then(|tok| match tok {
                Value::String(_) => Some(tok.clone()),
                Value::Number(n) if n.is_i64() || n.is_u64() => Some(tok.clone()),
                _ => None,
            });
        let progress: Option<ProgressEmitter> = progress_token.map(|token| {
            let transport = Arc::clone(&self.transport);
            Arc::new(
                move |progress: f64, total: Option<f64>, message: Option<&str>| {
                    // rmcp `ProgressNotificationParam` field order:
                    // progressToken, progress, total?, message?.
                    let mut p = Map::new();
                    p.insert("progressToken".to_string(), token.clone());
                    p.insert("progress".to_string(), json!(progress));
                    if let Some(total) = total {
                        p.insert("total".to_string(), json!(total));
                    }
                    if let Some(message) = message {
                        p.insert("message".to_string(), Value::String(message.to_string()));
                    }
                    transport.notify("notifications/progress", Some(Value::Object(p)));
                },
            ) as ProgressEmitter
        });

        // EXCEEDS TS: track this request so `notifications/cancelled` can set
        // its cooperative cancel flag while the engine works (rmcp
        // `local_ct_pool`). `initialize` never registers here — it is never
        // cancellable.
        let cancel = Arc::new(AtomicBool::new(false));
        let key = request_id_key(id);
        self.in_flight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.clone(), Arc::clone(&cancel));

        let result = self.engine.execute_with_context(
            &tool_name,
            tool_args,
            progress,
            Some(Arc::clone(&cancel)),
        );

        self.in_flight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key);

        // Cancelled mid-flight: per spec the response to a cancelled request
        // is suppressed — send nothing.
        if cancel.load(Ordering::SeqCst) {
            return;
        }

        let result_value = result
            .into_mcp_projection()
            .and_then(serde_json::to_value)
            .unwrap_or(Value::Null);
        self.transport.send_result(id, result_value);

        // A project may have opened (or caught up) during this call — if that
        // changed the tool list the client last saw, tell it (EXCEEDS TS;
        // rmcp `notify_tool_list_changed`).
        self.maybe_notify_tools_list_changed();
    }

    /// Reader-thread hook for `notifications/cancelled`: flag the in-flight
    /// request if we know it; unknown/late ids are tolerated silently, same
    /// as before (spec MUST).
    fn handle_cancelled_notification(&self, params: Option<&Value>) {
        let Some(request_id) = params.and_then(|p| p.get("requestId")) else {
            return;
        };
        let key = request_id_key(request_id);
        if let Some(flag) = self
            .in_flight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            flag.store(true, Ordering::SeqCst);
        }
    }

    /// `logging/setLevel` (EXCEEDS TS): set this session's minimum mirrored
    /// log level (rmcp `LoggingLevel`, lowercase) and ack with `{}`.
    fn handle_set_level(&self, request: &IncomingMessage) {
        let Some(id) = &request.id else { return };
        let level = request
            .params
            .as_ref()
            .and_then(|p| p.get("level"))
            .and_then(|l| l.as_str());
        match level.and_then(logging_level_rank) {
            Some(rank) => {
                self.log_subscription.set_min_rank(rank);
                self.transport.send_result(id, json!({}));
            }
            None => {
                self.transport.send_error(
                    id,
                    ErrorCodes::INVALID_PARAMS,
                    &format!("Invalid logging level: {}", level.unwrap_or("<missing>")),
                    None,
                );
            }
        }
    }

    /// Emit `notifications/tools/list_changed` when the engine's current tool
    /// list differs from the one this client last received (EXCEEDS TS).
    /// No-op until the client has listed at least once — there is nothing to
    /// invalidate before that.
    fn maybe_notify_tools_list_changed(&self) {
        let current = match serde_json::to_string(&self.engine.get_tools()) {
            Ok(serialized) => serialized,
            Err(_) => return,
        };
        let changed = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            match &state.last_listed_tools {
                Some(prev) if *prev != current => {
                    state.last_listed_tools = Some(current);
                    true
                }
                _ => false,
            }
        };
        if changed {
            // rmcp `ToolListChangedNotification` is param-less.
            self.transport
                .notify("notifications/tools/list_changed", None);
        }
    }

    /// Lazy default-project resolution. Three layers:
    ///   1. await the in-flight init kicked off from `handle_initialize`
    ///      (if any);
    ///   2. if still uninitialized and we never asked the client for its
    ///      roots, do so now (one-shot); fall back to cwd if the client lacks
    ///      roots;
    ///   3. last-resort: re-walk from the best candidate — picks up projects
    ///      that were `codegraph init`'d *after* the server started.
    fn retry_init_if_needed(&self) {
        let pending = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.resolve_pending.take()
        };
        if let Some(rx) = pending {
            let _ = rx.recv(); // failures fall through to retry
        }

        if self.engine.has_default_code_graph() {
            return;
        }

        let hint = self
            .explicit_project_path
            .clone()
            .or_else(|| self.engine.get_project_path());

        let (need_roots_attempt, supports_roots) = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            // `roots_refresh_requested` (set by notifications/roots/list_changed
            // while unresolved) re-arms the attempt even when an earlier
            // fallback recorded a useless hint (EXCEEDS TS).
            if (hint.is_none() || state.roots_refresh_requested) && !state.roots_attempted {
                state.roots_attempted = true;
                state.roots_refresh_requested = false;
                (true, state.client_supports_roots)
            } else {
                (false, state.client_supports_roots)
            }
        };
        if need_roots_attempt {
            if supports_roots {
                self.init_from_roots();
            } else {
                self.engine.ensure_initialized(&process_cwd());
            }
            if self.engine.has_default_code_graph() {
                return;
            }
        }

        // Last resort: walk from the best candidate (sync open). Picks up
        // projects that appeared after the server started.
        let candidate = hint.unwrap_or_else(process_cwd);
        self.engine.retry_initialize_sync(&candidate);
    }

    /// Ask the client for its workspace root via `roots/list` and open the
    /// first one. Falls back to `process_cwd()` on timeout or empty answer.
    fn init_from_roots(&self) {
        let mut target = process_cwd();
        match self
            .transport
            .request("roots/list", None, Some(ROOTS_LIST_TIMEOUT_MS))
        {
            Ok(result) => match first_root_path(&result) {
                Some(root_path) => target = root_path,
                None => {
                    self.log(
                        "warning",
                        "Client returned no workspace roots; falling back to process cwd.",
                    );
                }
            },
            Err(msg) => {
                self.log(
                    "warning",
                    &format!("roots/list request failed ({msg}); falling back to process cwd."),
                );
            }
        }
        self.engine.ensure_initialized(&target);
    }

    /// Session-side diagnostic: keep the exact `[CodeGraph MCP]` stderr line
    /// AND mirror it to this session as `notifications/message` (EXCEEDS TS —
    /// `logging` capability).
    fn log(&self, level: &str, message: &str) {
        eprintln!("[CodeGraph MCP] {message}");
        self.log_subscription.notify(level, message);
    }
}

/// Stable map key for a JSON-RPC request id (string OR integer ids — both
/// spec-legal; serialization disambiguates `1` from `"1"`).
fn request_id_key(id: &Value) -> String {
    serde_json::to_string(id).unwrap_or_default()
}

/// JS template-literal stringification for the "Unknown tool" edge where the
/// client sent a truthy non-string tool name.
fn stringify_js(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(_) | Value::Object(_) => "[object Object]".to_string(),
        Value::Null => "null".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiates_known_versions_and_defaults_to_latest() {
        // Unknown / newer → latest.
        assert_eq!(
            negotiated_protocol_version(Some(&json!("2025-11-25"))),
            "2025-06-18"
        );
        // Known older version → echoed.
        assert_eq!(
            negotiated_protocol_version(Some(&json!("2024-11-05"))),
            "2024-11-05"
        );
        assert_eq!(
            negotiated_protocol_version(Some(&json!("2025-03-26"))),
            "2025-03-26"
        );
        // Non-string / absent → latest.
        assert_eq!(negotiated_protocol_version(Some(&json!(42))), "2025-06-18");
        assert_eq!(negotiated_protocol_version(None), "2025-06-18");
    }

    #[test]
    fn server_info_shape() {
        let info = server_info();
        assert_eq!(info["name"], "codegraph");
        assert_eq!(info["version"], CODEGRAPH_PACKAGE_VERSION);
        // Key order: name then version (proxy handshake parity).
        assert_eq!(
            serde_json::to_string(&info).unwrap(),
            format!("{{\"name\":\"codegraph\",\"version\":\"{CODEGRAPH_PACKAGE_VERSION}\"}}")
        );
    }

    #[test]
    fn file_uri_to_path_decodes_and_resolves() {
        #[cfg(unix)]
        {
            assert_eq!(file_uri_to_path("file:///tmp/proj"), "/tmp/proj");
            assert_eq!(
                file_uri_to_path("file:///tmp/with%20space"),
                "/tmp/with space"
            );
            // Trailing slash collapses through path resolution.
            assert_eq!(file_uri_to_path("file:///tmp/proj/"), "/tmp/proj");
        }
    }

    #[test]
    fn first_root_path_extracts_the_first_usable_uri() {
        #[cfg(unix)]
        {
            let result = json!({ "roots": [{ "uri": "file:///tmp/proj", "name": "proj" }] });
            assert_eq!(first_root_path(&result), Some("/tmp/proj".to_string()));
        }
        assert_eq!(first_root_path(&json!({})), None);
        assert_eq!(first_root_path(&json!({ "roots": [] })), None);
        assert_eq!(first_root_path(&json!({ "roots": [{ "uri": 42 }] })), None);
        assert_eq!(first_root_path(&Value::Null), None);
    }

    #[test]
    fn percent_decode_handles_utf8_and_malformed_sequences() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("%E2%9C%93"), "✓");
        // Malformed escapes pass through (lenient deviation, documented).
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }
}

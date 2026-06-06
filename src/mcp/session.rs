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

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crossbeam_channel::Receiver;
use serde_json::{Map, Value, json};

use crate::mcp::engine::EngineHandle;
use crate::mcp::server_instructions::SERVER_INSTRUCTIONS;
use crate::mcp::tools::tools;
use crate::mcp::transport::{ErrorCodes, IncomingMessage, JsonRpcTransport};
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

/// JS truthiness (`!!value`) for the few spots the TS session relies on it.
fn js_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0 && !f.is_nan()).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

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
    /// In-flight background init kicked off from `handle_initialize`
    /// (TS `resolvePromise`).
    resolve_pending: Option<Receiver<()>>,
}

struct SessionInner {
    transport: Arc<dyn JsonRpcTransport>,
    engine: EngineHandle,
    explicit_project_path: Option<String>,
    state: Mutex<SessionState>,
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
        MCPSession {
            inner: Arc::new(SessionInner {
                transport,
                engine,
                explicit_project_path: opts.explicit_project_path,
                state: Mutex::new(SessionState {
                    client_supports_roots: false,
                    roots_attempted: false,
                    resolve_pending: None,
                }),
            }),
        }
    }

    /// Start handling messages from the transport. Returns immediately — the
    /// session lives for as long as the transport is open.
    pub fn start(&self) {
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
            "initialized" => {
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
        result.insert("capabilities".to_string(), json!({ "tools": {} }));
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
        let tools_value =
            serde_json::to_value(self.engine.get_tools()).unwrap_or(Value::Array(vec![]));
        if let Some(id) = &request.id {
            self.transport
                .send_result(id, json!({ "tools": tools_value }));
        }
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

        let result = self.engine.execute(&tool_name, tool_args);
        let result_value = serde_json::to_value(&result).unwrap_or(Value::Null);
        self.transport.send_result(id, result_value);
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
            if hint.is_none() && !state.roots_attempted {
                state.roots_attempted = true;
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
                    eprintln!(
                        "[CodeGraph MCP] Client returned no workspace roots; falling back to process cwd."
                    );
                }
            },
            Err(msg) => {
                eprintln!(
                    "[CodeGraph MCP] roots/list request failed ({msg}); falling back to process cwd."
                );
            }
        }
        self.engine.ensure_initialized(&target);
    }
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

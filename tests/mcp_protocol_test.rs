//! MCP protocol-conformance tests for the rmcp gap fixes (see
//! `notes/rmcp-gaps.md`, "Implemented divergences").
//!
//! Real spawned server processes over stdio (same harness pattern as
//! `tests/mcp_server_test.rs` — no mocks, tempfile projects, real SQLite):
//!
//! - MUST-FIX 1: proxy degraded mode answers EVERY request (`-32601` default
//!   arm, `-32700` parse-error recovery) — exercised by planting a
//!   wrong-version daemon socket so the local-handshake proxy goes degraded.
//! - MUST-FIX 2: `notifications/initialized` (spec spelling) is tolerated.
//! - SHOULD-ADDs: tool annotations, `tools.listChanged` +
//!   `notifications/tools/list_changed`, `_meta.progressToken` →
//!   `notifications/progress`, `notifications/cancelled` response
//!   suppression, `logging` capability (`logging/setLevel` +
//!   `notifications/message`), `notifications/roots/list_changed` latch
//!   re-arm. (The request-timeout `notifications/cancelled` lives in
//!   `src/mcp/transport.rs` unit tests.)

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard};
use std::time::{Duration, Instant};

use codegraph::{CodeGraph, InitOptions};
use serde_json::{Value, json};
use tempfile::TempDir;

static ENV_LOCK: RwLock<()> = RwLock::new(());

fn env_read() -> RwLockReadGuard<'static, ()> {
    ENV_LOCK.read().unwrap_or_else(|e| e.into_inner())
}

// =============================================================================
// Spawned-server harness (same shape as tests/mcp_server_test.rs)
// =============================================================================

#[derive(Debug, Clone)]
struct StreamEvent {
    stream: &'static str, // "stdout" | "stderr"
    text: String,
}

struct ServerProc {
    child: Child,
    stdin: Option<ChildStdin>,
    events: Arc<Mutex<Vec<StreamEvent>>>,
}

impl ServerProc {
    fn send(&mut self, msg: &Value) {
        let line = serde_json::to_string(msg).unwrap();
        self.send_raw(&line);
    }

    /// Write one raw line (used for the parse-error case).
    fn send_raw(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("server stdin");
        stdin.write_all(line.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    }

    fn events(&self) -> Vec<StreamEvent> {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Every stdout line that parses as JSON.
    fn messages(&self) -> Vec<Value> {
        self.events()
            .iter()
            .filter(|e| e.stream == "stdout")
            .filter_map(|e| serde_json::from_str(e.text.trim()).ok())
            .collect()
    }
}

impl Drop for ServerProc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_server(cwd: &Path, args: &[&str], no_daemon: bool) -> ServerProc {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_codegraph-mcp-server"));
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("CODEGRAPH_NO_WATCH")
        .env_remove("CODEGRAPH_FORCE_WATCH")
        .env_remove("CODEGRAPH_NO_DAEMON")
        .env_remove("CODEGRAPH_DAEMON_INTERNAL")
        .env_remove("CODEGRAPH_MCP_TOOLS")
        .env_remove("CODEGRAPH_MCP_DEBUG")
        .env_remove("CODEGRAPH_WATCH_DEBOUNCE_MS")
        .env_remove("CODEGRAPH_PPID_POLL_MS")
        .env_remove("NODE_ENV")
        .env_remove("VITEST");
    if no_daemon {
        cmd.env("CODEGRAPH_NO_DAEMON", "1");
    }
    let mut child = cmd.spawn().expect("spawn codegraph-mcp-server");

    let events: Arc<Mutex<Vec<StreamEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let seq = Arc::new(AtomicUsize::new(0));

    let stdout = child.stdout.take().expect("child stdout");
    {
        let events = Arc::clone(&events);
        let seq = Arc::clone(&seq);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let _ = seq.fetch_add(1, Ordering::SeqCst);
                events
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(StreamEvent {
                        stream: "stdout",
                        text: line,
                    });
            }
        });
    }
    let stderr = child.stderr.take().expect("child stderr");
    {
        let events = Arc::clone(&events);
        let seq = Arc::clone(&seq);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                let _ = seq.fetch_add(1, Ordering::SeqCst);
                events
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(StreamEvent {
                        stream: "stderr",
                        text: line,
                    });
            }
        });
    }

    let stdin = child.stdin.take();
    ServerProc {
        child,
        stdin,
        events,
    }
}

fn wait_for_message(proc_: &ServerProc, timeout: Duration, pred: impl Fn(&Value) -> bool) -> Value {
    let started = Instant::now();
    loop {
        if let Some(hit) = proc_.messages().into_iter().find(|m| pred(m)) {
            return hit;
        }
        if started.elapsed() > timeout {
            panic!("Timed out. Messages so far: {:?}", proc_.messages());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn initialize_msg(
    project_path: Option<&Path>,
    protocol_version: &str,
    capabilities: Value,
) -> Value {
    let mut params = serde_json::Map::new();
    params.insert("protocolVersion".to_string(), json!(protocol_version));
    params.insert("capabilities".to_string(), capabilities);
    params.insert(
        "clientInfo".to_string(),
        json!({ "name": "test", "version": "0.0.0" }),
    );
    if let Some(p) = project_path {
        params.insert(
            "rootUri".to_string(),
            json!(format!("file://{}", p.display())),
        );
    }
    json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": Value::Object(params) })
}

fn init_project(dir: &Path) {
    let cg = CodeGraph::init(dir, &InitOptions::default()).expect("CodeGraph.init");
    cg.close();
}

/// The annotation set every tool must carry (rmcp ToolAnnotations camelCase).
fn expected_annotations() -> Value {
    json!({
        "readOnlyHint": true,
        "destructiveHint": false,
        "idempotentHint": true,
        "openWorldHint": false,
    })
}

// =============================================================================
// MUST-FIX 2 — notifications/initialized (spec spelling) is a real no-op arm
// =============================================================================

#[test]
fn tolerates_notifications_initialized_in_both_spellings() {
    let _guard = env_read();
    let tmp = TempDir::new().unwrap();
    let mut server = spawn_server(tmp.path(), &["--no-watch"], true);
    server.send(&initialize_msg(Some(tmp.path()), "2025-06-18", json!({})));
    wait_for_message(&server, Duration::from_secs(5), |m| m["id"] == 0);

    server.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    server.send(&json!({ "jsonrpc": "2.0", "method": "initialized" }));
    // A follow-up ping still round-trips (the stream is healthy)...
    server.send(&json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }));
    let pong = wait_for_message(&server, Duration::from_secs(5), |m| m["id"] == 1);
    assert_eq!(pong["result"], json!({}));
    // ...and neither spelling produced an error (notifications are never
    // answered — a -32601 here would mean the arm regressed).
    assert!(
        server.messages().iter().all(|m| m.get("error").is_none()),
        "no error may be emitted for initialized notifications: {:?}",
        server.messages()
    );
}

#[test]
fn unknown_request_method_gets_method_not_found() {
    let _guard = env_read();
    let tmp = TempDir::new().unwrap();
    let mut server = spawn_server(tmp.path(), &["--no-watch"], true);
    server.send(&initialize_msg(Some(tmp.path()), "2025-06-18", json!({})));
    wait_for_message(&server, Duration::from_secs(5), |m| m["id"] == 0);

    server.send(&json!({ "jsonrpc": "2.0", "id": 9, "method": "bogus/method" }));
    let err = wait_for_message(&server, Duration::from_secs(5), |m| m["id"] == 9);
    assert_eq!(err["error"]["code"], -32601);
    assert_eq!(err["error"]["message"], "Method not found: bogus/method");
}

// =============================================================================
// SHOULD-ADD 1+2 — capabilities + tool annotations + listChanged notification
// =============================================================================

#[test]
fn initialize_advertises_list_changed_and_logging() {
    let _guard = env_read();
    let tmp = TempDir::new().unwrap();
    let mut server = spawn_server(tmp.path(), &["--no-watch"], true);
    server.send(&initialize_msg(Some(tmp.path()), "2025-06-18", json!({})));
    let init = wait_for_message(&server, Duration::from_secs(5), |m| m["id"] == 0);
    assert_eq!(
        init["result"]["capabilities"],
        json!({ "logging": {}, "tools": { "listChanged": true } })
    );
}

#[test]
fn every_tool_carries_read_only_annotations() {
    let _guard = env_read();
    let tmp = TempDir::new().unwrap();
    let mut server = spawn_server(tmp.path(), &["--no-watch"], true);
    server.send(&initialize_msg(Some(tmp.path()), "2025-06-18", json!({})));
    wait_for_message(&server, Duration::from_secs(5), |m| m["id"] == 0);

    server.send(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} }));
    let listed = wait_for_message(&server, Duration::from_secs(5), |m| m["id"] == 1);
    let tools = listed["result"]["tools"].as_array().expect("tools array");
    assert!(!tools.is_empty());
    for tool in tools {
        assert_eq!(
            tool["annotations"],
            expected_annotations(),
            "tool {} must carry the read-only annotation set",
            tool["name"]
        );
    }
}

#[test]
fn static_tools_fn_carries_annotations_too() {
    // The proxy's static tools/list answer serializes get_static_tools() —
    // annotations must ride along for free.
    let _guard = env_read();
    let tools = serde_json::to_value(codegraph::mcp::tools::get_static_tools()).unwrap();
    for tool in tools.as_array().unwrap() {
        assert_eq!(tool["annotations"], expected_annotations());
    }
}

#[test]
fn emits_tools_list_changed_when_a_late_project_open_changes_the_list() {
    let _guard = env_read();
    let tmp = TempDir::new().unwrap();
    // NO project yet: tools/list serves the full static surface (13 tools).
    let mut server = spawn_server(tmp.path(), &["--no-watch"], true);
    server.send(&initialize_msg(None, "2025-06-18", json!({})));
    wait_for_message(&server, Duration::from_secs(5), |m| m["id"] == 0);

    server.send(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} }));
    let first = wait_for_message(&server, Duration::from_secs(8), |m| m["id"] == 1);
    assert_eq!(first["result"]["tools"].as_array().unwrap().len(), 13);

    // The project appears AFTER the server started (and after the client
    // listed). The next tool call resolves it (retry_initialize_sync), the
    // tiny-repo gating shrinks the list, and the session must announce it.
    init_project(tmp.path());
    server.send(&json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": { "name": "codegraph_status", "arguments": {} }
    }));
    wait_for_message(&server, Duration::from_secs(15), |m| m["id"] == 2);
    wait_for_message(&server, Duration::from_secs(5), |m| {
        m["method"] == "notifications/tools/list_changed"
    });

    // Re-listing now returns the gated (tiny-repo) list.
    server.send(&json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list", "params": {} }));
    let second = wait_for_message(&server, Duration::from_secs(8), |m| m["id"] == 3);
    let names: Vec<&str> = second["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert_eq!(
        names,
        ["codegraph_search", "codegraph_node", "codegraph_explore"],
        "tiny-repo gating must shrink the list to the core trio"
    );
}

// =============================================================================
// SHOULD-ADD 3 — _meta.progressToken → notifications/progress
// =============================================================================

#[test]
fn emits_progress_for_a_token_bearing_first_call_and_never_unsolicited() {
    let _guard = env_read();
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(
        tmp.path().join("src/a.ts"),
        "export function alpha() { return 1; }\n",
    )
    .unwrap();
    init_project(tmp.path());

    let mut server = spawn_server(tmp.path(), &["--no-watch"], true);
    server.send(&initialize_msg(Some(tmp.path()), "2025-06-18", json!({})));
    wait_for_message(&server, Duration::from_secs(5), |m| m["id"] == 0);

    // First call carries a progressToken — the catch-up sync reports through it.
    server.send(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {
            "name": "codegraph_status",
            "arguments": {},
            "_meta": { "progressToken": "tok-1" }
        }
    }));
    wait_for_message(&server, Duration::from_secs(20), |m| m["id"] == 1);

    let progress: Vec<Value> = server
        .messages()
        .into_iter()
        .filter(|m| m["method"] == "notifications/progress")
        .collect();
    assert!(
        !progress.is_empty(),
        "a token-bearing first call must emit progress"
    );
    for p in &progress {
        assert_eq!(p["params"]["progressToken"], "tok-1");
        assert!(p["params"]["progress"].is_number());
    }
    // The final emission marks completion (total == progress).
    let last = progress.last().unwrap();
    assert_eq!(last["params"]["message"], "Catch-up sync complete");
    assert_eq!(last["params"]["total"], last["params"]["progress"]);

    // A token-less call emits nothing further (never unsolicited).
    let before = progress.len();
    server.send(&json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": { "name": "codegraph_status", "arguments": {} }
    }));
    wait_for_message(&server, Duration::from_secs(10), |m| m["id"] == 2);
    let after = server
        .messages()
        .iter()
        .filter(|m| m["method"] == "notifications/progress")
        .count();
    assert_eq!(after, before, "no token ⇒ no progress notifications");
}

// =============================================================================
// SHOULD-ADD 4 — notifications/cancelled suppresses the in-flight response
// =============================================================================

#[test]
fn cancelled_tools_call_gets_no_response() {
    let _guard = env_read();
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    for i in 0..1200 {
        let mut content = String::new();
        for j in 0..25 {
            content.push_str(&format!(
                "export function fn_{i}_{j}(x: number) {{ return x + {j}; }}\n"
            ));
        }
        std::fs::write(src.join(format!("mod_{i}.ts")), content).unwrap();
    }
    init_project(tmp.path());

    let mut server = spawn_server(tmp.path(), &["--no-watch"], true);
    server.send(&initialize_msg(Some(tmp.path()), "2025-06-18", json!({})));
    wait_for_message(&server, Duration::from_secs(5), |m| m["id"] == 0);

    server.send(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {
            "name": "codegraph_status",
            "arguments": {},
            "_meta": { "progressToken": "cancel-1" }
        }
    }));
    wait_for_message(&server, Duration::from_secs(20), |m| {
        m["method"] == "notifications/progress"
            && m["params"]["progressToken"] == "cancel-1"
            && m["params"]["message"] != "Catch-up sync complete"
    });
    server.send(&json!({
        "jsonrpc": "2.0", "method": "notifications/cancelled",
        "params": { "requestId": 1, "reason": "user aborted" }
    }));

    // The stream stays serviceable: a follow-up ping answers…
    server.send(&json!({ "jsonrpc": "2.0", "id": 2, "method": "ping" }));
    wait_for_message(&server, Duration::from_secs(60), |m| m["id"] == 2);
    // …and the cancelled call's response was suppressed (spec SHOULD).
    assert!(
        !server.messages().iter().any(|m| m["id"] == 1),
        "response to the cancelled request must be suppressed: {:?}",
        server
            .messages()
            .iter()
            .filter(|m| m["id"] == 1)
            .collect::<Vec<_>>()
    );
}

#[test]
fn late_or_unknown_cancellations_are_tolerated() {
    let _guard = env_read();
    let tmp = TempDir::new().unwrap();
    let mut server = spawn_server(tmp.path(), &["--no-watch"], true);
    server.send(&initialize_msg(Some(tmp.path()), "2025-06-18", json!({})));
    wait_for_message(&server, Duration::from_secs(5), |m| m["id"] == 0);

    // Cancel an id that was never in flight — silently ignored.
    server.send(&json!({
        "jsonrpc": "2.0", "method": "notifications/cancelled",
        "params": { "requestId": 12345 }
    }));
    server.send(&json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }));
    let pong = wait_for_message(&server, Duration::from_secs(5), |m| m["id"] == 1);
    assert_eq!(pong["result"], json!({}));
    assert!(server.messages().iter().all(|m| m.get("error").is_none()));
}

// =============================================================================
// SHOULD-ADD 5 — logging capability
// =============================================================================

#[test]
fn logging_set_level_acks_with_empty_result_and_rejects_garbage() {
    let _guard = env_read();
    let tmp = TempDir::new().unwrap();
    let mut server = spawn_server(tmp.path(), &["--no-watch"], true);
    server.send(&initialize_msg(Some(tmp.path()), "2025-06-18", json!({})));
    wait_for_message(&server, Duration::from_secs(5), |m| m["id"] == 0);

    server.send(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "logging/setLevel",
        "params": { "level": "debug" }
    }));
    let ack = wait_for_message(&server, Duration::from_secs(5), |m| m["id"] == 1);
    assert_eq!(ack["result"], json!({}));

    server.send(&json!({
        "jsonrpc": "2.0", "id": 2, "method": "logging/setLevel",
        "params": { "level": "verbose" }
    }));
    let err = wait_for_message(&server, Duration::from_secs(5), |m| m["id"] == 2);
    assert_eq!(err["error"]["code"], -32602);
    assert!(
        err["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Invalid logging level")
    );
}

#[test]
fn mirrors_watcher_diagnostics_as_notifications_message() {
    let _guard = env_read();
    let tmp = TempDir::new().unwrap();
    init_project(tmp.path());

    // Watch stays ENABLED: "File watcher active" (info) is the deterministic
    // engine diagnostic we expect to see mirrored.
    let mut server = spawn_server(tmp.path(), &[], true);
    // No rootUri: the project opens lazily during the first tool call, well
    // after the session's log subscription is registered.
    server.send(&initialize_msg(None, "2025-06-18", json!({})));
    wait_for_message(&server, Duration::from_secs(5), |m| m["id"] == 0);
    server.send(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "codegraph_status", "arguments": {} }
    }));
    wait_for_message(&server, Duration::from_secs(15), |m| m["id"] == 1);

    let log = wait_for_message(&server, Duration::from_secs(10), |m| {
        m["method"] == "notifications/message"
            && m["params"]["data"]
                .as_str()
                .is_some_and(|d| d.contains("File watcher active"))
    });
    assert_eq!(log["params"]["level"], "info");
    assert_eq!(log["params"]["logger"], "codegraph");
}

#[test]
fn set_level_filters_below_threshold_messages() {
    let _guard = env_read();
    let tmp = TempDir::new().unwrap();
    init_project(tmp.path());

    let mut server = spawn_server(tmp.path(), &[], true);
    server.send(&initialize_msg(None, "2025-06-18", json!({})));
    wait_for_message(&server, Duration::from_secs(5), |m| m["id"] == 0);

    // Raise the floor BEFORE the project opens; the info-level watcher line
    // must then be stderr-only.
    server.send(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "logging/setLevel",
        "params": { "level": "emergency" }
    }));
    wait_for_message(&server, Duration::from_secs(5), |m| m["id"] == 1);

    server.send(&json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": { "name": "codegraph_status", "arguments": {} }
    }));
    wait_for_message(&server, Duration::from_secs(15), |m| m["id"] == 2);

    // stderr still carries the diagnostic (bytes unchanged)…
    let started = Instant::now();
    while !server
        .events()
        .iter()
        .any(|e| e.stream == "stderr" && e.text.contains("File watcher active"))
    {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "watcher must have started: {:?}",
            server.events()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // …but nothing below `emergency` is mirrored to the client.
    assert!(
        !server
            .messages()
            .iter()
            .any(|m| m["method"] == "notifications/message"),
        "info diagnostics must be filtered at level=emergency"
    );
}

// =============================================================================
// SHOULD-ADD 6 — notifications/roots/list_changed re-arms the roots latch
// =============================================================================

#[test]
fn roots_list_changed_re_arms_the_one_shot_roots_query() {
    let _guard = env_read();
    let cwd_dir = TempDir::new().unwrap(); // no project here
    let project_dir = TempDir::new().unwrap();

    let mut server = spawn_server(cwd_dir.path(), &["--no-watch"], false);
    server.send(&initialize_msg(None, "2025-06-18", json!({ "roots": {} })));
    wait_for_message(&server, Duration::from_secs(5), |m| {
        m["id"] == 0 && m.get("result").is_some()
    });
    server.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));

    // First call: server asks for roots; we answer EMPTY → fallback fails.
    server.send(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "codegraph_status", "arguments": {} }
    }));
    let roots_req1 = wait_for_message(&server, Duration::from_secs(5), |m| {
        m["method"] == "roots/list"
    });
    server.send(&json!({
        "jsonrpc": "2.0", "id": roots_req1["id"], "result": { "roots": [] }
    }));
    let resp1 = wait_for_message(&server, Duration::from_secs(8), |m| m["id"] == 1);
    assert!(
        resp1["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("No CodeGraph project is loaded")
    );

    // The workspace gains a project; the host announces its roots changed.
    init_project(project_dir.path());
    server.send(&json!({ "jsonrpc": "2.0", "method": "notifications/roots/list_changed" }));

    // Next call must RE-ASK for roots (a second roots/list with a new id)…
    server.send(&json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": { "name": "codegraph_status", "arguments": {} }
    }));
    let roots_req2 = wait_for_message(&server, Duration::from_secs(5), |m| {
        m["method"] == "roots/list" && m["id"] != roots_req1["id"]
    });
    server.send(&json!({
        "jsonrpc": "2.0", "id": roots_req2["id"],
        "result": { "roots": [{ "uri": format!("file://{}", project_dir.path().display()), "name": "proj" }] }
    }));

    // …and the project resolves this time.
    let resp2 = wait_for_message(&server, Duration::from_secs(15), |m| m["id"] == 2);
    let text = resp2["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("CodeGraph Status"), "got: {text}");
}

// =============================================================================
// MUST-FIX 1 — proxy degraded mode answers every request
// (unix-gated: the local-handshake proxy is the unix daemon path)
// =============================================================================

#[cfg(unix)]
mod degraded_proxy {
    use std::os::unix::net::UnixListener;

    use codegraph::mcp::daemon_paths::get_daemon_socket_path;

    use super::*;

    /// Plant a wrong-version "daemon" on the project's real socket path so
    /// the spawned proxy goes degraded immediately (VersionMismatch is
    /// definitive — no polling, no daemon spawn).
    fn plant_mismatched_daemon(project_root: &Path) -> std::thread::JoinHandle<()> {
        let canonical = std::fs::canonicalize(project_root).unwrap();
        let socket_path = get_daemon_socket_path(&canonical);
        if socket_path.exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
        let listener = UnixListener::bind(&socket_path).expect("bind fake daemon socket");
        std::thread::spawn(move || {
            // Serve a few hellos in case of retries; exit with the test.
            for _ in 0..4 {
                let Ok((mut conn, _)) = listener.accept() else {
                    return;
                };
                let _ = conn.write_all(
                    b"{\"codegraph\":\"0.0.0-mismatch\",\"pid\":1,\"socketPath\":\"x\",\"protocol\":1}\n",
                );
                std::thread::sleep(Duration::from_millis(100));
            }
        })
    }

    #[test]
    fn degraded_proxy_answers_every_request_and_recovers_from_parse_errors() {
        let _guard = env_read();
        let tmp = TempDir::new().unwrap();
        init_project(tmp.path());
        let _fake_daemon = plant_mismatched_daemon(tmp.path());

        // no_daemon = false → the local-handshake proxy path; the planted
        // wrong-version daemon forces Failed (degraded, in-process).
        let mut server = spawn_server(tmp.path(), &[], false);
        server.send(&initialize_msg(Some(tmp.path()), "2025-06-18", json!({})));
        let init = wait_for_message(&server, Duration::from_secs(10), |m| m["id"] == 0);
        // Locally-answered handshake advertises the same capabilities as the
        // daemon session would.
        assert_eq!(
            init["result"]["capabilities"],
            json!({ "logging": {}, "tools": { "listChanged": true } })
        );

        // 1. Unknown request → -32601 with the session's exact string
        //    (previously: silently dropped, host hung).
        server.send(&json!({ "jsonrpc": "2.0", "id": 2, "method": "some/unknown" }));
        let err = wait_for_message(&server, Duration::from_secs(10), |m| m["id"] == 2);
        assert_eq!(err["error"]["code"], -32601);
        assert_eq!(err["error"]["message"], "Method not found: some/unknown");

        // 2. Unparseable line → -32700 with id:null; the stream stays alive
        //    (previously: silently dropped).
        server.send_raw("this is not json {{{");
        let parse_err = wait_for_message(&server, Duration::from_secs(10), |m| {
            m["error"]["code"] == -32700
        });
        assert_eq!(parse_err["id"], Value::Null);
        assert_eq!(parse_err["error"]["message"], "Parse error: invalid JSON");

        // 3. logging/setLevel is advertised → acked in degraded mode too.
        server.send(&json!({
            "jsonrpc": "2.0", "id": 3, "method": "logging/setLevel",
            "params": { "level": "warning" }
        }));
        let ack = wait_for_message(&server, Duration::from_secs(10), |m| m["id"] == 3);
        assert_eq!(ack["result"], json!({}));

        // 4. ping still answers; notifications still get nothing.
        server.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        server.send(&json!({ "jsonrpc": "2.0", "id": 4, "method": "ping" }));
        let pong = wait_for_message(&server, Duration::from_secs(10), |m| m["id"] == 4);
        assert_eq!(pong["result"], json!({}));

        // 5. tools/list (static answer in Failed state) carries annotations.
        server.send(&json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/list", "params": {} }));
        let listed = wait_for_message(&server, Duration::from_secs(10), |m| m["id"] == 5);
        let tools = listed["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 13);
        for tool in tools {
            assert_eq!(tool["annotations"], expected_annotations());
        }

        // 6. tools/call executes in-process (degraded still serves).
        server.send(&json!({
            "jsonrpc": "2.0", "id": 6, "method": "tools/call",
            "params": { "name": "codegraph_status", "arguments": {} }
        }));
        let result = wait_for_message(&server, Duration::from_secs(30), |m| m["id"] == 6);
        assert!(
            result["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("CodeGraph Status")
        );
    }
}

// =============================================================================
// CallContext (cancel/progress plumbing) — in-process coverage
// =============================================================================

#[test]
fn pre_cancelled_context_short_circuits_tool_execution() {
    use std::sync::atomic::AtomicBool;

    use codegraph::mcp::tools::ToolHandler;

    let _guard = env_read();
    let handler = ToolHandler::new(None);
    let ctx = handler.call_context();
    let flag = Arc::new(AtomicBool::new(true));
    ctx.set(None, Some(Arc::clone(&flag)));
    let res = handler.execute("codegraph_search", &json!({ "query": "anything" }));
    assert_eq!(res.is_error, Some(true));
    assert!(res.content[0].text.contains("Request cancelled"));
    ctx.clear();

    // Cleared context: the same call proceeds to normal handling (here, the
    // "no project" error — NOT the cancellation marker).
    let res2 = handler.execute("codegraph_search", &json!({ "query": "anything" }));
    assert!(!res2.content[0].text.contains("Request cancelled"));
}

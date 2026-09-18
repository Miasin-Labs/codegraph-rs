//! Spawned-process protocol coverage for the rmcp-backed CodeGraph server.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use codegraph::{CodeGraph, InitOptions};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::{RwLock, RwLockReadGuard};

static ENV_LOCK: RwLock<()> = RwLock::const_new(());

async fn env_read() -> RwLockReadGuard<'static, ()> {
    ENV_LOCK.read().await
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

async fn init_project(dir: &Path) {
    let cg = CodeGraph::init(dir, &InitOptions::default())
        .await
        .expect("CodeGraph.init");
    cg.close();
}

fn explore_evidence(response: &Value) -> HashSet<(String, u64, u64, String)> {
    response["result"]["structuredContent"]["sourceFiles"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|file| {
            let path = file["path"].as_str().unwrap().to_string();
            file["chunks"]
                .as_array()
                .into_iter()
                .flatten()
                .map(move |chunk| {
                    (
                        path.clone(),
                        chunk["startLine"].as_u64().unwrap(),
                        chunk["endLine"].as_u64().unwrap(),
                        chunk["source"].as_str().unwrap().to_string(),
                    )
                })
        })
        .collect()
}

/// The default tool surface, in catalog order.
const DEFAULT_TOOLS: [&str; 12] = [
    "codegraph_search",
    "codegraph_callers",
    "codegraph_callees",
    "codegraph_impact",
    "codegraph_node",
    "codegraph_explore",
    "codegraph_status",
    "codegraph_files",
    "codegraph_history",
    "codegraph_tests",
    "codegraph_diagnostics",
    "codegraph_grep",
];

/// The annotation set a tool must carry (rmcp ToolAnnotations camelCase):
/// every tool is a read over the index except `diagnostics`, which runs the
/// project's compiler and so writes build artifacts.
fn expected_annotations(tool: &Value) -> Value {
    json!({
        "readOnlyHint": tool["name"] != "codegraph_diagnostics",
        "destructiveHint": false,
        "idempotentHint": true,
        "openWorldHint": false,
    })
}

// =============================================================================
// MUST-FIX 2 — notifications/initialized (spec spelling) is a real no-op arm
// =============================================================================

#[tokio::test(flavor = "current_thread")]
async fn negotiates_rmcp_current_protocol_version() {
    let _guard = env_read().await;
    let tmp = TempDir::new().unwrap();
    let mut server = spawn_server(tmp.path(), &["--no-watch"], true);
    server.send(&initialize_msg(Some(tmp.path()), "2025-11-25", json!({})));

    let init = wait_for_message(&server, Duration::from_secs(5), |m| m["id"] == 0);

    assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
}

#[tokio::test(flavor = "current_thread")]
async fn tolerates_notifications_initialized_in_both_spellings() {
    let _guard = env_read().await;
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

#[tokio::test(flavor = "current_thread")]
async fn unknown_request_method_gets_method_not_found() {
    let _guard = env_read().await;
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

#[tokio::test(flavor = "current_thread")]
async fn initialize_advertises_list_changed_and_logging() {
    let _guard = env_read().await;
    let tmp = TempDir::new().unwrap();
    let mut server = spawn_server(tmp.path(), &["--no-watch"], true);
    server.send(&initialize_msg(Some(tmp.path()), "2025-06-18", json!({})));
    let init = wait_for_message(&server, Duration::from_secs(5), |m| m["id"] == 0);
    assert_eq!(
        init["result"]["capabilities"],
        json!({ "logging": {}, "tools": { "listChanged": true } })
    );
}

#[tokio::test(flavor = "current_thread")]
async fn every_tool_carries_its_annotations() {
    let _guard = env_read().await;
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
            expected_annotations(tool),
            "tool {} must carry its annotation set",
            tool["name"]
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn tools_list_defaults_to_the_core_tools() {
    let _guard = env_read().await;
    let tmp = TempDir::new().unwrap();
    let mut server = spawn_server(tmp.path(), &["--no-watch"], true);
    server.send(&initialize_msg(Some(tmp.path()), "2025-11-25", json!({})));
    wait_for_message(&server, Duration::from_secs(5), |message| {
        message["id"] == 0
    });

    server.send(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} }));
    let listed = wait_for_message(&server, Duration::from_secs(5), |message| {
        message["id"] == 1
    });
    let names = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(names, DEFAULT_TOOLS);
    let explore = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "codegraph_explore")
        .expect("codegraph_explore");
    let query_description = explore["inputSchema"]["properties"]["query"]["description"]
        .as_str()
        .unwrap();
    assert!(query_description.contains("no prior codegraph_search needed"));
    assert!(!query_description.contains("Use codegraph_search first"));
}

#[tokio::test(flavor = "current_thread")]
async fn sequential_explore_calls_reallocate_to_fresh_evidence() {
    let _guard = env_read().await;
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    for file in 0..8 {
        let mut source = format!("export class SessionWorkflow{file} {{\n");
        for method in 0..12 {
            source.push_str(&format!(
                "  sessionWorkflow{file}Step{method}(value: string): string {{ return value + '{file}-{method}'; }}\n"
            ));
        }
        source.push_str("}\n");
        std::fs::write(src.join(format!("session{file}.ts")), source).unwrap();
    }
    init_project(project.path()).await;
    let mut server = spawn_server(project.path(), &["--no-watch"], true);
    server.send(&initialize_msg(
        Some(project.path()),
        "2025-11-25",
        json!({}),
    ));
    wait_for_message(&server, Duration::from_secs(5), |message| {
        message["id"] == 0
    });

    for id in 1..=5 {
        server.send(&json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {
                "name": "codegraph_explore",
                "arguments": { "query": "session workflow", "maxFiles": 2 }
            }
        }));
        wait_for_message(&server, Duration::from_secs(20), |message| {
            message["id"] == id
        });
    }
    let evidence = (1..=5)
        .map(|id| {
            let response = wait_for_message(&server, Duration::from_secs(1), |message| {
                message["id"] == id
            });
            explore_evidence(&response)
        })
        .collect::<Vec<_>>();
    let second = wait_for_message(&server, Duration::from_secs(1), |message| {
        message["id"] == 2
    });

    assert!(evidence.iter().all(|call| !call.is_empty()));
    for calls in evidence[..4].windows(2) {
        assert!(calls[0].is_disjoint(&calls[1]));
    }
    assert!(
        !second["result"]["structuredContent"]["backReferences"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        evidence.iter().map(HashSet::len).collect::<Vec<_>>(),
        [2, 2, 2, 2, 1]
    );
    println!(
        "allocation_counts={:?}",
        evidence.iter().map(HashSet::len).collect::<Vec<_>>()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn primary_lookup_tools_advertise_output_schemas() {
    let _guard = env_read().await;
    let tmp = TempDir::new().unwrap();
    let mut server = spawn_server(tmp.path(), &["--no-watch"], true);
    server.send(&initialize_msg(Some(tmp.path()), "2025-06-18", json!({})));
    wait_for_message(&server, Duration::from_secs(5), |m| m["id"] == 0);

    server.send(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} }));
    let listed = wait_for_message(&server, Duration::from_secs(5), |m| m["id"] == 1);
    let tools = listed["result"]["tools"].as_array().expect("tools array");
    let explore = tools
        .iter()
        .find(|tool| tool["name"] == "codegraph_explore")
        .expect("codegraph_explore");
    assert!(explore.get("outputSchema").is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn static_tools_fn_carries_annotations_too() {
    // The proxy's static tools/list answer serializes get_static_tools() —
    // annotations must ride along for free.
    let _guard = env_read().await;
    let tools = serde_json::to_value(codegraph::mcp::tools::get_static_tools()).unwrap();
    for tool in tools.as_array().unwrap() {
        assert_eq!(tool["annotations"], expected_annotations(tool));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn emits_tools_list_changed_when_a_late_project_open_changes_the_list() {
    let _guard = env_read().await;
    let tmp = TempDir::new().unwrap();
    let mut server = spawn_server(tmp.path(), &["--no-watch"], true);
    server.send(&initialize_msg(None, "2025-06-18", json!({})));
    wait_for_message(&server, Duration::from_secs(5), |m| m["id"] == 0);

    server.send(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} }));
    let first = wait_for_message(&server, Duration::from_secs(8), |m| m["id"] == 1);
    assert_eq!(
        first["result"]["tools"].as_array().unwrap().len(),
        DEFAULT_TOOLS.len()
    );

    // The project appears AFTER the server started (and after the client
    // listed). The next tool call resolves it (retry_initialize_sync), the
    // explore description gains the project's call budget, and the session
    // must announce the changed list.
    init_project(tmp.path()).await;
    server.send(&json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": { "name": "codegraph_status", "arguments": {} }
    }));
    wait_for_message(&server, Duration::from_secs(15), |m| m["id"] == 2);
    wait_for_message(&server, Duration::from_secs(5), |m| {
        m["method"] == "notifications/tools/list_changed"
    });

    // Re-listing keeps the same tools; only the descriptions changed.
    server.send(&json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list", "params": {} }));
    let second = wait_for_message(&server, Duration::from_secs(8), |m| m["id"] == 3);
    let names: Vec<&str> = second["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert_eq!(
        names, DEFAULT_TOOLS,
        "the default surface remains stable after project discovery"
    );
}

// =============================================================================
// SHOULD-ADD 3 — _meta.progressToken → notifications/progress
// =============================================================================

#[tokio::test(flavor = "current_thread")]
async fn emits_progress_for_a_token_bearing_first_call_and_never_unsolicited() {
    let _guard = env_read().await;
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(
        tmp.path().join("src/a.ts"),
        "export function alpha() { return 1; }\n",
    )
    .unwrap();
    init_project(tmp.path()).await;

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

#[tokio::test(flavor = "current_thread")]
async fn cancelled_tools_call_gets_no_response() {
    let _guard = env_read().await;
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
    init_project(tmp.path()).await;

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

#[tokio::test(flavor = "current_thread")]
async fn late_or_unknown_cancellations_are_tolerated() {
    let _guard = env_read().await;
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

#[tokio::test(flavor = "current_thread")]
async fn logging_set_level_acks_with_empty_result_and_rejects_garbage() {
    let _guard = env_read().await;
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

#[tokio::test(flavor = "current_thread")]
async fn mirrors_watcher_diagnostics_as_notifications_message() {
    let _guard = env_read().await;
    let tmp = TempDir::new().unwrap();
    init_project(tmp.path()).await;

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

#[tokio::test(flavor = "current_thread")]
async fn set_level_filters_below_threshold_messages() {
    let _guard = env_read().await;
    let tmp = TempDir::new().unwrap();
    init_project(tmp.path()).await;

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

#[tokio::test(flavor = "current_thread")]
async fn roots_list_changed_re_arms_the_one_shot_roots_query() {
    let _guard = env_read().await;
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
    let text1 = resp1["result"]["content"][0]["text"]
        .as_str()
        .expect("JSON text content");
    let parsed1: Value = serde_json::from_str(text1).expect("content text is compact JSON");
    assert_eq!(parsed1, resp1["result"]["structuredContent"]);
    assert_eq!(parsed1["kind"], "error");
    assert_eq!(parsed1["error"]["code"], "tool_error");

    // The workspace gains a project; the host announces its roots changed.
    init_project(project_dir.path()).await;
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
    let text2 = resp2["result"]["content"][0]["text"]
        .as_str()
        .expect("JSON text content");
    let parsed2: Value = serde_json::from_str(text2).expect("content text is compact JSON");
    assert_eq!(parsed2, resp2["result"]["structuredContent"]);
    assert_eq!(parsed2["kind"], "status");
}

// =============================================================================
// Version-mismatch fallback stays on the same rmcp implementation
// =============================================================================

#[cfg(unix)]
mod direct_fallback {
    use std::os::unix::net::UnixListener;

    use codegraph::mcp::daemon_paths::get_daemon_socket_path;

    use super::*;

    fn plant_mismatched_daemon(project_root: &Path) -> std::thread::JoinHandle<()> {
        let canonical = std::fs::canonicalize(project_root).unwrap();
        let socket_path = get_daemon_socket_path(&canonical);
        if socket_path.exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
        let listener = UnixListener::bind(&socket_path).expect("bind fake daemon socket");
        std::thread::spawn(move || {
            let Ok((mut conn, _)) = listener.accept() else {
                return;
            };
            let _ = conn.write_all(
                b"{\"codegraph\":\"0.0.0-mismatch\",\"pid\":1,\"socketPath\":\"x\",\"protocol\":1}\n",
            );
        })
    }

    #[tokio::test(flavor = "current_thread")]
    async fn version_mismatch_falls_back_to_rmcp_and_survives_bad_input() {
        let _guard = env_read().await;
        let tmp = TempDir::new().unwrap();
        init_project(tmp.path()).await;
        let fake_daemon = plant_mismatched_daemon(tmp.path());

        let mut server = spawn_server(tmp.path(), &[], false);
        server.send(&initialize_msg(Some(tmp.path()), "2025-11-25", json!({})));
        let init = wait_for_message(&server, Duration::from_secs(10), |m| m["id"] == 0);
        fake_daemon.join().expect("fake daemon exits after hello");
        assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(
            init["result"]["capabilities"],
            json!({ "logging": {}, "tools": { "listChanged": true } })
        );

        server.send(&json!({ "jsonrpc": "2.0", "id": 2, "method": "some/unknown" }));
        let err = wait_for_message(&server, Duration::from_secs(10), |m| m["id"] == 2);
        assert_eq!(err["error"]["code"], -32601);
        assert_eq!(err["error"]["message"], "Method not found: some/unknown");

        server.send_raw("this is not json {{{");
        server.send(&json!({
            "jsonrpc": "2.0", "id": 3, "method": "logging/setLevel",
            "params": { "level": "warning" }
        }));
        let ack = wait_for_message(&server, Duration::from_secs(10), |m| m["id"] == 3);
        assert_eq!(ack["result"], json!({}));

        server.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        server.send(&json!({ "jsonrpc": "2.0", "id": 4, "method": "ping" }));
        let pong = wait_for_message(&server, Duration::from_secs(10), |m| m["id"] == 4);
        assert_eq!(pong["result"], json!({}));
        assert!(
            server
                .messages()
                .iter()
                .all(|message| message["error"]["code"] != -32700)
        );

        server.send(&json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/list", "params": {} }));
        let listed = wait_for_message(&server, Duration::from_secs(10), |m| m["id"] == 5);
        let tools = listed["result"]["tools"].as_array().unwrap();
        assert_eq!(
            tools
                .iter()
                .filter_map(|tool| tool["name"].as_str())
                .collect::<Vec<_>>(),
            DEFAULT_TOOLS
        );
        for tool in tools {
            assert_eq!(tool["annotations"], expected_annotations(tool));
        }

        server.send(&json!({
            "jsonrpc": "2.0", "id": 6, "method": "tools/call",
            "params": { "name": "codegraph_status", "arguments": {} }
        }));
        let result = wait_for_message(&server, Duration::from_secs(30), |m| m["id"] == 6);
        let text = result["result"]["content"][0]["text"]
            .as_str()
            .expect("JSON text content");
        let parsed: Value = serde_json::from_str(text).expect("content text is compact JSON");
        assert_eq!(parsed, result["result"]["structuredContent"]);
        assert_eq!(result["result"]["structuredContent"]["kind"], "status");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fallback_tool_results_use_compact_json_projection() {
        let _guard = env_read().await;
        let tmp = TempDir::new().unwrap();
        init_project(tmp.path()).await;
        let fake_daemon = plant_mismatched_daemon(tmp.path());
        let mut server = spawn_server(tmp.path(), &[], false);
        server.send(&initialize_msg(Some(tmp.path()), "2025-11-25", json!({})));
        wait_for_message(&server, Duration::from_secs(10), |m| m["id"] == 0);
        fake_daemon.join().expect("fake daemon exits after hello");

        server.send(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "codegraph_not_a_tool", "arguments": {} }
        }));
        let unknown = wait_for_message(&server, Duration::from_secs(10), |m| m["id"] == 1);
        server.send(&json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": { "name": "codegraph_status", "arguments": {} }
        }));
        let response = wait_for_message(&server, Duration::from_secs(30), |m| m["id"] == 2);

        assert_eq!(unknown["error"]["code"], -32602);
        assert_eq!(
            unknown["error"]["message"],
            "Unknown tool: codegraph_not_a_tool"
        );
        assert!(unknown.get("result").is_none());
        assert!(unknown.get("content").is_none());
        assert!(unknown.get("structuredContent").is_none());
        let result = &response["result"];
        let content = result["content"].as_array().expect("tool result content");
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        let text = content[0]["text"].as_str().expect("JSON text content");
        let parsed: Value = serde_json::from_str(text).expect("content text is compact JSON");
        assert!(
            parsed.is_object(),
            "JSON must not be nested or double-encoded"
        );
        assert_eq!(parsed, result["structuredContent"]);
        println!("degraded_result_json={result}");
    }
}

// =============================================================================
// CallContext (cancel/progress plumbing) — in-process coverage
// =============================================================================

#[tokio::test(flavor = "current_thread")]
async fn pre_cancelled_context_short_circuits_tool_execution() {
    use std::sync::atomic::AtomicBool;

    use codegraph::mcp::tools::ToolHandler;

    let _guard = env_read().await;
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

// =============================================================================
// codegraph_grep and the per-connection session ledger
// =============================================================================

fn call_structured(server: &mut ServerProc, id: u64, name: &str, arguments: Value) -> Value {
    server.send(&json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": { "name": name, "arguments": arguments }
    }));
    let response = wait_for_message(server, Duration::from_secs(20), |message| {
        message["id"] == id
    });
    response["result"]["structuredContent"].clone()
}

/// Line numbers of a grep file row's `N: text` hit lines.
fn hit_lines(file: &Value) -> Vec<u64> {
    file["hits"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|group| group["lines"].as_array().unwrap().clone())
        .map(|line| {
            let line = line.as_str().unwrap().to_string();
            line.split_once(':').unwrap().0.parse().unwrap()
        })
        .collect()
}

/// `codegraph_grep` joins the session ledger: repeating a search leaves out
/// the hits it already sent (listing their lines) and shows the next ones,
/// lines a `node` read sent verbatim count too, and grep's one-line
/// excerpts never make `node` treat those lines as already sent.
#[tokio::test(flavor = "current_thread")]
async fn grep_leaves_out_hits_the_session_already_received() {
    let _guard = env_read().await;
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let mut source =
        String::from("export function audit(): string[] {\n  const out: string[] = [];\n");
    for step in 0..6 {
        source.push_str(&format!("  out.push('AUDIT_EVENT step {step}');\n"));
    }
    source.push_str("  return out;\n}\n");
    std::fs::write(src.join("audit.ts"), &source).unwrap();
    init_project(project.path()).await;
    let mut server = spawn_server(project.path(), &["--no-watch"], true);
    server.send(&initialize_msg(
        Some(project.path()),
        "2025-11-25",
        json!({}),
    ));
    wait_for_message(&server, Duration::from_secs(5), |message| {
        message["id"] == 0
    });
    let search = json!({ "pattern": "AUDIT_EVENT", "maxPerFile": 2 });

    let first = call_structured(&mut server, 1, "codegraph_grep", search.clone());
    let file = &first["files"][0];
    assert_eq!(file["count"], 6, "{first}");
    assert_eq!(hit_lines(file), [3, 4]);
    assert_eq!(file["hits"][0]["symbol"], "audit");
    assert!(file.get("alreadySent").is_none());

    let second = call_structured(&mut server, 2, "codegraph_grep", search.clone());
    let file = &second["files"][0];
    assert_eq!(hit_lines(file), [5, 6], "{second}");
    assert_eq!(file["alreadySent"], json!([3, 4]));

    // Grep's excerpts are not source: node still sends lines 3-4 in full.
    let view = call_structured(
        &mut server,
        3,
        "codegraph_node",
        json!({ "file": "src/audit.ts", "offset": 3, "limit": 6 }),
    );
    assert!(view["source"].is_string(), "{view}");
    assert!(view.get("alreadySent").is_none(), "{view}");

    // Everything is now in the session: a third search sends no hit text.
    let third = call_structured(&mut server, 4, "codegraph_grep", search);
    let file = &third["files"][0];
    assert!(hit_lines(file).is_empty(), "{third}");
    assert_eq!(file["alreadySent"], json!([3, 4, 5, 6, 7, 8]));
    assert_eq!(file["count"], 6);
    assert!(
        file.get("more").is_none(),
        "the listed lines are all of them"
    );
}

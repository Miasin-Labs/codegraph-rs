//! MCP server integration tests.
//!
//! Ports (real files, real SQLite, real spawned server processes — no mocks):
//! - `__tests__/mcp-initialize.test.ts` (issue #172 handshake contract, #621
//!   resources/prompts probes)
//! - `__tests__/mcp-roots.test.ts` (issue #196 roots/list project resolution)
//! - `__tests__/mcp-staleness-banner.test.ts` (issue #403 per-file staleness)
//! - `__tests__/mcp-catchup-gate.test.ts` (server-side remainder — the gate
//!   lives in ToolHandler, the engine pokes it; see notes/mcp-daemon.md)
//! - `__tests__/security.test.ts` "MCP Input Validation" describe (deferred
//!   from the foundation wave per notes/ui.md)
//!
//! The TS suites spawn `dist/bin/codegraph.js serve --mcp`; here we spawn the
//! `codegraph-mcp-server` helper binary (same `MCPServer` entry the CLI's
//! `serve --mcp` will construct) over stdio via `CARGO_BIN_EXE_*`.
//!
//! Env-var discipline: tests that MUTATE process env (NODE_ENV for the
//! watcher test seam) take the ENV_LOCK write lock; everything else takes the
//! read lock — same pattern as tests/sync_test.rs / tests/mcp_tools_test.rs.
//! Spawned children additionally get the relevant CODEGRAPH_* vars pinned via
//! `env_remove`/`env` so parallel in-process env mutation can't leak in.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};

use codegraph::mcp::tools::{ToolHandler, ToolResult};
use codegraph::sync::{WatchOptions, emit_watch_event_for_tests};
use codegraph::{CodeGraph, IndexOptions, InitOptions};
use serde_json::{Value, json};
use tempfile::TempDir;

static ENV_LOCK: RwLock<()> = RwLock::new(());

fn env_read() -> RwLockReadGuard<'static, ()> {
    ENV_LOCK.read().unwrap_or_else(|e| e.into_inner())
}

fn env_write() -> RwLockWriteGuard<'static, ()> {
    ENV_LOCK.write().unwrap_or_else(|e| e.into_inner())
}

/// Sets an env var for the test's duration, restoring the prior value on drop
/// (vitest afterEach parity).
struct EnvVarGuard {
    key: String,
    original: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &str, value: &str) -> Self {
        let original = std::env::var(key).ok();
        std::env::set_var(key, value);
        EnvVarGuard {
            key: key.to_string(),
            original,
        }
    }

    fn unset(key: &str) -> Self {
        let original = std::env::var(key).ok();
        std::env::remove_var(key);
        EnvVarGuard {
            key: key.to_string(),
            original,
        }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(v) => std::env::set_var(&self.key, v),
            None => std::env::remove_var(&self.key),
        }
    }
}

// =============================================================================
// Spawned-server harness (TS `spawnServer` + `tagStreams`/`collectMessages`)
// =============================================================================

#[derive(Debug, Clone)]
struct StreamEvent {
    seq: usize,
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
        let stdin = self.stdin.as_mut().expect("server stdin");
        let line = serde_json::to_string(msg).unwrap();
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

    /// Every stdout line that parses as JSON (TS `collectMessages`).
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

/// Spawn the MCP server binary with its cwd in `cwd`. Mirrors the TS
/// `spawnServer` helpers: `no_daemon` pins direct (in-process) mode via
/// `CODEGRAPH_NO_DAEMON=1`; extra args (e.g. `--no-watch`) pass through.
/// The CODEGRAPH_*/test-runtime env vars are pinned so parallel in-process
/// env-mutating tests can't leak into the child.
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
                let n = seq.fetch_add(1, Ordering::SeqCst);
                events
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(StreamEvent {
                        seq: n,
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
                let n = seq.fetch_add(1, Ordering::SeqCst);
                events
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(StreamEvent {
                        seq: n,
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

fn wait_for_event(
    proc_: &ServerProc,
    timeout: Duration,
    pred: impl Fn(&StreamEvent) -> bool,
) -> StreamEvent {
    let started = Instant::now();
    loop {
        if let Some(hit) = proc_.events().iter().find(|e| pred(e)) {
            return hit.clone();
        }
        if started.elapsed() > timeout {
            panic!(
                "Timed out waiting for predicate. Events: {:?}",
                proc_.events()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
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

fn wait_for(cond: impl Fn() -> bool, timeout_ms: u64) {
    let started = Instant::now();
    while !cond() {
        if started.elapsed() > Duration::from_millis(timeout_ms) {
            panic!("waitFor timed out");
        }
        std::thread::sleep(Duration::from_millis(25));
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

fn first_text(result: &ToolResult) -> &str {
    &result.content[0].text
}

// =============================================================================
// SERVER_INSTRUCTIONS — byte parity with the TS single source of truth (#529)
// =============================================================================

#[test]
fn server_instructions_are_byte_identical_to_the_ts_source() {
    // The TS tree lives elsewhere now that the crate is standalone — point
    // CODEGRAPH_TS_REPO at a checkout of colbymchenry/codegraph to enable this
    // parity check; falls back to the old in-repo layout (../src).
    let ts_path = match std::env::var_os("CODEGRAPH_TS_REPO") {
        Some(repo) => Path::new(&repo).join("src/mcp/server-instructions.ts"),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/mcp/server-instructions.ts"),
    };
    let Ok(ts) = std::fs::read_to_string(&ts_path) else {
        // Repo layout without the TS tree (e.g. crate published standalone) —
        // nothing to compare against.
        return;
    };
    let marker = "SERVER_INSTRUCTIONS = `";
    let start = ts.find(marker).expect("TS template start") + marker.len();
    let end = ts.rfind("`;").expect("TS template end");
    // Unescape the template-literal escapes used in the file (backticks).
    let expected = ts[start..end]
        .replace("\\`", "`")
        .replace("\\${", "${")
        .replace("\\\\", "\\");
    assert_eq!(
        codegraph::mcp::server_instructions::SERVER_INSTRUCTIONS,
        expected,
        "server_instructions.rs must stay byte-identical to server-instructions.ts (issue #529)"
    );
}

// =============================================================================
// MCP initialize handshake (issue #172) — __tests__/mcp-initialize.test.ts
// =============================================================================

#[test]
fn responds_to_initialize_quickly_when_no_codegraph_exists_in_cwd() {
    let _guard = env_read();
    let tmp = TempDir::new().unwrap();
    let mut server = spawn_server(tmp.path(), &[], true);
    server.send(&initialize_msg(Some(tmp.path()), "2025-11-25", json!({})));

    let response = wait_for_event(&server, Duration::from_secs(5), |e| e.stream == "stdout");
    let parsed: Value = serde_json::from_str(&response.text).unwrap();
    assert_eq!(parsed["jsonrpc"], "2.0");
    assert_eq!(parsed["id"], 0);
    assert!(!parsed["result"]["protocolVersion"].is_null());
    assert!(parsed["result"]["capabilities"]["tools"].is_object());
}

#[test]
fn advertises_the_2025_06_18_mcp_protocol_to_newer_clients() {
    let _guard = env_read();
    let tmp = TempDir::new().unwrap();
    let mut server = spawn_server(tmp.path(), &[], true);
    server.send(&initialize_msg(Some(tmp.path()), "2025-11-25", json!({})));

    let response = wait_for_event(&server, Duration::from_secs(5), |e| e.stream == "stdout");
    let parsed: Value = serde_json::from_str(&response.text).unwrap();
    assert_eq!(parsed["result"]["protocolVersion"], "2025-06-18");
}

#[test]
fn negotiates_down_to_a_known_older_client_protocol() {
    let _guard = env_read();
    let tmp = TempDir::new().unwrap();
    let mut server = spawn_server(tmp.path(), &[], true);
    server.send(&initialize_msg(Some(tmp.path()), "2024-11-05", json!({})));

    let response = wait_for_event(&server, Duration::from_secs(5), |e| e.stream == "stdout");
    let parsed: Value = serde_json::from_str(&response.text).unwrap();
    assert_eq!(parsed["result"]["protocolVersion"], "2024-11-05");
}

#[test]
fn sends_initialize_response_before_try_initialize_default_finishes() {
    let _guard = env_read();
    let tmp = TempDir::new().unwrap();
    // Seed a real .codegraph so the server's init path runs its full body:
    // CodeGraph::open() and then start_watching() (which logs "File watcher
    // active" to stderr). That stderr log is observable evidence that the
    // default-project init has completed. The contract we're protecting: the
    // JSON-RPC response on stdout must arrive BEFORE that stderr log.
    init_project(tmp.path());

    let mut server = spawn_server(tmp.path(), &[], true);
    server.send(&initialize_msg(Some(tmp.path()), "2025-11-25", json!({})));

    let response = wait_for_event(&server, Duration::from_secs(10), |e| e.stream == "stdout");
    let watcher_log = wait_for_event(&server, Duration::from_secs(10), |e| {
        e.stream == "stderr" && e.text.contains("File watcher active")
    });
    assert!(
        response.seq < watcher_log.seq,
        "initialize response (seq {}) must precede the watcher log (seq {})",
        response.seq,
        watcher_log.seq
    );
    let parsed: Value = serde_json::from_str(&response.text).unwrap();
    assert_eq!(parsed["id"], 0);
    assert_eq!(parsed["result"]["serverInfo"]["name"], "codegraph");
}

#[test]
fn answers_resources_list_and_prompts_list_with_empty_lists_not_32601() {
    let _guard = env_read();
    let tmp = TempDir::new().unwrap();
    let mut server = spawn_server(tmp.path(), &[], true);
    server.send(&initialize_msg(Some(tmp.path()), "2025-11-25", json!({})));
    wait_for_event(&server, Duration::from_secs(5), |e| e.stream == "stdout"); // initialize reply

    server.send(&json!({ "jsonrpc": "2.0", "id": 1, "method": "resources/list", "params": {} }));
    server.send(&json!({ "jsonrpc": "2.0", "id": 2, "method": "prompts/list", "params": {} }));

    let resources = wait_for_message(&server, Duration::from_secs(5), |m| m["id"] == 1);
    assert!(resources.get("error").is_none());
    assert_eq!(resources["result"]["resources"], json!([]));

    let prompts = wait_for_message(&server, Duration::from_secs(5), |m| m["id"] == 2);
    assert!(prompts.get("error").is_none());
    assert_eq!(prompts["result"]["prompts"], json!([]));
}

// =============================================================================
// MCP project resolution via roots/list (issue #196) — __tests__/mcp-roots.test.ts
// =============================================================================

#[test]
fn resolves_the_project_from_the_client_roots_list_when_no_root_uri_is_sent() {
    let _guard = env_read();
    let cwd_dir = TempDir::new().unwrap(); // where the server is launched — has NO .codegraph
    let project_dir = TempDir::new().unwrap(); // the real indexed project the client reports
    init_project(project_dir.path());

    // --no-watch keeps the test deterministic and avoids watcher startup noise.
    let mut server = spawn_server(cwd_dir.path(), &["--no-watch"], false);

    // Advertise the roots capability but pass NO rootUri/workspaceFolders.
    server.send(&initialize_msg(None, "2025-11-25", json!({ "roots": {} })));
    wait_for_message(&server, Duration::from_secs(5), |m| {
        m["id"] == 0 && m.get("result").is_some()
    });
    server.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));

    // First tool call (no projectPath) drives the server to ask us for roots.
    server.send(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "codegraph_status", "arguments": {} }
    }));

    let roots_req = wait_for_message(&server, Duration::from_secs(5), |m| {
        m["method"] == "roots/list"
    });
    assert!(roots_req["id"].is_string(), "server-initiated id"); // server-initiated id
    server.send(&json!({
        "jsonrpc": "2.0", "id": roots_req["id"],
        "result": { "roots": [{ "uri": format!("file://{}", project_dir.path().display()), "name": "proj" }] }
    }));

    // The status call now succeeds against the resolved project.
    let resp = wait_for_message(&server, Duration::from_secs(8), |m| m["id"] == 1);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("CodeGraph Status"));
    assert!(!text.contains("No CodeGraph project is loaded"));
}

#[test]
fn returns_an_actionable_error_when_there_is_no_root_uri_and_no_roots_capability() {
    let _guard = env_read();
    let cwd_dir = TempDir::new().unwrap();
    let mut server = spawn_server(cwd_dir.path(), &["--no-watch"], false);

    server.send(&initialize_msg(None, "2025-11-25", json!({})));
    wait_for_message(&server, Duration::from_secs(5), |m| {
        m["id"] == 0 && m.get("result").is_some()
    });
    server.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));

    server.send(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "codegraph_status", "arguments": {} }
    }));
    let resp = wait_for_message(&server, Duration::from_secs(8), |m| m["id"] == 1);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();

    assert!(text.contains("No CodeGraph project is loaded"));
    assert!(text.contains("projectPath"));
    assert!(text.contains("--path"));
    // Names the directory it actually searched (the wrong cwd) so the user
    // can see why detection missed. basename survives any symlink realpath-ing.
    let basename = cwd_dir
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert!(text.contains(&basename));
    // It must not have hung waiting on roots/list — the client never offered it.
    assert!(
        !server
            .messages()
            .iter()
            .any(|m| m["method"] == "roots/list")
    );
}

#[test]
fn honors_an_explicit_root_uri_without_asking_the_client_for_roots() {
    let _guard = env_read();
    let cwd_dir = TempDir::new().unwrap();
    let project_dir = TempDir::new().unwrap();
    init_project(project_dir.path());

    let mut server = spawn_server(cwd_dir.path(), &["--no-watch"], false);

    server.send(&initialize_msg(
        Some(project_dir.path()),
        "2025-11-25",
        json!({ "roots": {} }),
    ));
    wait_for_message(&server, Duration::from_secs(5), |m| {
        m["id"] == 0 && m.get("result").is_some()
    });
    server.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));

    server.send(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "codegraph_status", "arguments": {} }
    }));
    let resp = wait_for_message(&server, Duration::from_secs(8), |m| m["id"] == 1);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();

    assert!(text.contains("CodeGraph Status"));
    // rootUri is a stronger signal than roots — we never needed to ask.
    assert!(
        !server
            .messages()
            .iter()
            .any(|m| m["method"] == "roots/list")
    );
}

// =============================================================================
// MCP staleness banner (issue #403) — __tests__/mcp-staleness-banner.test.ts
// =============================================================================

/// Fixture: three isolated files with no cross-references — keeps each test's
/// "which path does the response mention?" assertion unambiguous.
fn staleness_fixture() -> (TempDir, Rc<CodeGraph>, ToolHandler) {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("alpha-only.ts"),
        "export function alphaOnly() { return 1; }\n",
    )
    .unwrap();
    std::fs::write(
        src.join("bravo-only.ts"),
        "export function bravoOnly() { return 2; }\n",
    )
    .unwrap();
    std::fs::write(
        src.join("charlie-only.ts"),
        "export function charlieOnly() { return 3; }\n",
    )
    .unwrap();

    let cg = Rc::new(CodeGraph::init_sync(tmp.path()).unwrap());
    cg.index_all(&IndexOptions::default()).unwrap();
    let handler = ToolHandler::new(Some(Rc::clone(&cg)));
    (tmp, cg, handler)
}

/// NODE_ENV=test gates the watcher's deterministic event seam
/// (`emit_watch_event_for_tests`); CODEGRAPH_NO_WATCH must not leak in from
/// parallel suites.
fn watcher_test_env() -> (EnvVarGuard, EnvVarGuard) {
    (
        EnvVarGuard::set("NODE_ENV", "test"),
        EnvVarGuard::unset("CODEGRAPH_NO_WATCH"),
    )
}

#[test]
fn prepends_a_stale_banner_when_the_response_references_a_pending_file() {
    let _lock = env_write();
    let _env = watcher_test_env();
    let (tmp, cg, handler) = staleness_fixture();

    // Long debounce so the edit lingers in pending files while we query.
    assert!(cg.watch(WatchOptions {
        debounce_ms: Some(4000),
        inert_for_tests: true,
        ..Default::default()
    }));
    cg.wait_until_watcher_ready(None).unwrap();

    // Real disk write so a later sync (if it fires) sees the new content,
    // plus a synthesized event so the watcher's pending set updates
    // immediately without waiting on OS-level event delivery.
    std::fs::write(
        tmp.path().join("src").join("alpha-only.ts"),
        "export function alphaOnly() { return 99; }\n",
    )
    .unwrap();
    let root_key = cg.get_project_root().to_string_lossy().to_string();
    assert!(emit_watch_event_for_tests(&root_key, "src/alpha-only.ts"));

    wait_for(
        || {
            cg.get_pending_files()
                .iter()
                .any(|p| p.path == "src/alpha-only.ts")
        },
        2000,
    );

    let res = handler.execute("codegraph_search", &json!({ "query": "alphaOnly" }));
    assert_ne!(res.is_error, Some(true));
    let text = first_text(&res);

    // Banner shape: warning glyph + filename + actionable instruction.
    assert!(
        text.starts_with("⚠️"),
        "banner must lead the response: {text}"
    );
    assert!(text.contains("src/alpha-only.ts"));
    assert!(
        regex::Regex::new(r"edited \d+ms ago")
            .unwrap()
            .is_match(text)
    );
    assert!(text.contains("Read them directly"));
    // The actual result must still follow the banner.
    assert!(text.contains("alphaOnly"));

    cg.unwatch();
    cg.close();
}

#[test]
fn uses_the_footer_not_the_banner_when_pending_files_are_not_referenced() {
    let _lock = env_write();
    let _env = watcher_test_env();
    let (tmp, cg, handler) = staleness_fixture();

    assert!(cg.watch(WatchOptions {
        debounce_ms: Some(4000),
        inert_for_tests: true,
        ..Default::default()
    }));
    cg.wait_until_watcher_ready(None).unwrap();

    // Edit bravo-only.ts but search for the alphaOnly symbol, whose hit is
    // only in alpha-only.ts. The two files share no imports/calls so the
    // response text won't mention bravo-only.ts.
    std::fs::write(
        tmp.path().join("src").join("bravo-only.ts"),
        "export function bravoOnly() { return 22; }\n",
    )
    .unwrap();
    let root_key = cg.get_project_root().to_string_lossy().to_string();
    assert!(emit_watch_event_for_tests(&root_key, "src/bravo-only.ts"));
    wait_for(
        || {
            cg.get_pending_files()
                .iter()
                .any(|p| p.path == "src/bravo-only.ts")
        },
        2000,
    );

    let res = handler.execute("codegraph_search", &json!({ "query": "alphaOnly" }));
    let text = first_text(&res);

    assert!(!text.starts_with("⚠️"));
    assert!(text.contains("elsewhere in this project are pending index sync"));
    assert!(text.contains("src/bravo-only.ts"));

    cg.unwatch();
    cg.close();
}

#[test]
fn drops_the_banner_once_the_sync_completes_and_clears_the_pending_entry() {
    let _lock = env_write();
    let _env = watcher_test_env();
    let (tmp, cg, handler) = staleness_fixture();

    assert!(cg.watch(WatchOptions {
        debounce_ms: Some(200),
        inert_for_tests: true,
        ..Default::default()
    }));
    cg.wait_until_watcher_ready(None).unwrap();

    std::fs::write(
        tmp.path().join("src").join("alpha-only.ts"),
        "export function alphaOnly() { return 7; }\n",
    )
    .unwrap();
    let root_key = cg.get_project_root().to_string_lossy().to_string();
    assert!(emit_watch_event_for_tests(&root_key, "src/alpha-only.ts"));
    // Wait through debounce (200ms) + sync; pending files drain back to empty.
    wait_for(|| cg.get_pending_files().is_empty(), 3000);

    let res = handler.execute("codegraph_search", &json!({ "query": "alphaOnly" }));
    let text = first_text(&res);
    assert!(!text.starts_with("⚠️"));
    assert!(!text.contains("elsewhere in this project are pending index sync"));

    cg.unwatch();
    cg.close();
}

#[test]
fn lists_pending_files_under_pending_sync_in_codegraph_status() {
    let _lock = env_write();
    let _env = watcher_test_env();
    let (tmp, cg, handler) = staleness_fixture();

    assert!(cg.watch(WatchOptions {
        debounce_ms: Some(4000),
        inert_for_tests: true,
        ..Default::default()
    }));
    cg.wait_until_watcher_ready(None).unwrap();

    std::fs::write(
        tmp.path().join("src").join("charlie-only.ts"),
        "export function charlieOnly() { return 33; }\n",
    )
    .unwrap();
    let root_key = cg.get_project_root().to_string_lossy().to_string();
    assert!(emit_watch_event_for_tests(&root_key, "src/charlie-only.ts"));
    wait_for(
        || {
            cg.get_pending_files()
                .iter()
                .any(|p| p.path == "src/charlie-only.ts")
        },
        2000,
    );

    let res = handler.execute("codegraph_status", &json!({}));
    let text = first_text(&res);
    assert!(text.contains("### Pending sync:"));
    assert!(text.contains("src/charlie-only.ts"));
    // Status embeds the info first-class, so the auto-banner is suppressed.
    assert!(!text.starts_with("⚠️"));

    cg.unwatch();
    cg.close();
}

#[test]
fn returns_zero_pending_files_when_no_watcher_is_active() {
    let _lock = env_read();
    let (_tmp, cg, _handler) = staleness_fixture();
    assert!(cg.get_pending_files().is_empty());
    cg.close();
}

// =============================================================================
// MCP catch-up gate — __tests__/mcp-catchup-gate.test.ts (server-side remainder)
// =============================================================================

fn catchup_fixture() -> (TempDir, Rc<CodeGraph>, ToolHandler) {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("survivor.ts"),
        "export function survivor() { return 1; }\n",
    )
    .unwrap();
    std::fs::write(
        src.join("deleted-later.ts"),
        "export function deletedLater() { return 2; }\n",
    )
    .unwrap();

    let cg = Rc::new(CodeGraph::init_sync(tmp.path()).unwrap());
    cg.index_all(&IndexOptions::default()).unwrap();
    let handler = ToolHandler::new(Some(Rc::clone(&cg)));
    (tmp, cg, handler)
}

#[test]
fn awaits_the_gate_before_serving_the_first_tool_call() {
    let _lock = env_read();
    let (_tmp, cg, handler) = catchup_fixture();

    let gate_resolved = Rc::new(std::cell::Cell::new(false));
    let flag = Rc::clone(&gate_resolved);
    handler.set_catch_up_gate(Some(Box::new(move || {
        std::thread::sleep(Duration::from_millis(80));
        flag.set(true);
    })));

    let res = handler.execute("codegraph_search", &json!({ "query": "survivor" }));
    assert!(gate_resolved.get());
    assert_ne!(res.is_error, Some(true));
    assert!(first_text(&res).contains("survivor"));
    cg.close();
}

#[test]
fn drops_the_gate_after_first_await_second_call_does_not_re_wait() {
    let _lock = env_read();
    let (_tmp, cg, handler) = catchup_fixture();

    let run_count = Rc::new(std::cell::Cell::new(0u32));
    let counter = Rc::clone(&run_count);
    handler.set_catch_up_gate(Some(Box::new(move || {
        counter.set(counter.get() + 1);
    })));

    handler.execute("codegraph_search", &json!({ "query": "survivor" }));
    let before = run_count.get();
    handler.execute("codegraph_search", &json!({ "query": "survivor" }));
    // The gate is one-shot: the second execute never re-runs it because the
    // gate field was cleared (TS nulled the awaited promise).
    assert_eq!(run_count.get(), before);
    assert_eq!(before, 1);
    cg.close();
}

#[test]
fn catch_up_reconciles_a_deleted_file_before_the_first_tool_call_sees_it() {
    let _lock = env_read();
    let (tmp, cg, handler) = catchup_fixture();

    // Simulate the empty-project / deleted-files startup case: file is in
    // the DB (we indexed it above) but vanishes from disk before the MCP
    // server's first query. The catch-up sync, run via the gate, must remove
    // the row so the first tool call returns no hit.
    std::fs::remove_file(tmp.path().join("src").join("deleted-later.ts")).unwrap();

    // Push the actual catch-up sync as the gate — same flow the MCP engine
    // uses (the engine's gate closure runs `cg.sync()` and swallows errors).
    let cg_for_gate = Rc::clone(&cg);
    handler.set_catch_up_gate(Some(Box::new(move || {
        let _ = cg_for_gate.sync(&IndexOptions::default());
    })));

    let res = handler.execute("codegraph_search", &json!({ "query": "deletedLater" }));
    assert_ne!(res.is_error, Some(true));
    assert!(!first_text(&res).contains("src/deleted-later.ts"));
    cg.close();
}

#[test]
fn catch_up_that_converges_the_project_to_0_files_clears_all_rows() {
    let _lock = env_read();
    let (tmp, cg, handler) = catchup_fixture();

    // Worst case: every source file is gone between sessions. Without the
    // gate, the first tool call serves whatever was in the DB. With the gate
    // + the orchestrator's filesystem reconcile, the DB drains.
    std::fs::remove_file(tmp.path().join("src").join("survivor.ts")).unwrap();
    std::fs::remove_file(tmp.path().join("src").join("deleted-later.ts")).unwrap();

    let cg_for_gate = Rc::clone(&cg);
    handler.set_catch_up_gate(Some(Box::new(move || {
        let _ = cg_for_gate.sync(&IndexOptions::default());
    })));

    let res = handler.execute("codegraph_search", &json!({ "query": "survivor" }));
    assert_ne!(res.is_error, Some(true));
    assert_eq!(cg.get_stats().unwrap().file_count, 0);
    cg.close();
}

#[test]
fn gate_that_fails_does_not_break_the_tool_call() {
    let _lock = env_read();
    let (_tmp, cg, handler) = catchup_fixture();

    // A catch-up sync failure (lock contention, transient FS error) must not
    // poison tool dispatch — the engine's gate closure logs and swallows the
    // error (TS: a rejected gate promise), the handler proceeds.
    handler.set_catch_up_gate(Some(Box::new(|| {
        let failed: Result<(), String> = Err("simulated sync failure".to_string());
        if let Err(err) = failed {
            // The engine logs this; the gate must not panic/propagate.
            let _ = err;
        }
    })));

    let res = handler.execute("codegraph_search", &json!({ "query": "survivor" }));
    assert_ne!(res.is_error, Some(true));
    assert!(first_text(&res).contains("survivor"));
    cg.close();
}

// =============================================================================
// MCP Input Validation — __tests__/security.test.ts (deferred to the MCP wave
// per notes/ui.md)
// =============================================================================

fn security_fixture() -> (TempDir, Rc<CodeGraph>, ToolHandler) {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("example.ts"),
        "export function exampleFunc(): void {}\nexport class ExampleClass {}\n",
    )
    .unwrap();

    let cg = Rc::new(CodeGraph::init_sync(tmp.path()).unwrap());
    cg.index_all(&IndexOptions::default()).unwrap();
    let handler = ToolHandler::new(Some(Rc::clone(&cg)));
    (tmp, cg, handler)
}

#[test]
fn rejects_non_string_query_in_codegraph_search() {
    let _lock = env_read();
    let (_tmp, cg, handler) = security_fixture();
    let res = handler.execute("codegraph_search", &json!({ "query": null }));
    assert_eq!(res.is_error, Some(true));
    assert!(first_text(&res).contains("non-empty string"));
    cg.close();
}

#[test]
fn rejects_empty_string_query_in_codegraph_search() {
    let _lock = env_read();
    let (_tmp, cg, handler) = security_fixture();
    let res = handler.execute("codegraph_search", &json!({ "query": "" }));
    assert_eq!(res.is_error, Some(true));
    assert!(first_text(&res).contains("non-empty string"));
    cg.close();
}

#[test]
fn accepts_valid_query_in_codegraph_search() {
    let _lock = env_read();
    let (_tmp, cg, handler) = security_fixture();
    let res = handler.execute("codegraph_search", &json!({ "query": "example" }));
    assert_ne!(res.is_error, Some(true));
    cg.close();
}

#[test]
fn clamps_limit_to_valid_range_in_codegraph_search() {
    let _lock = env_read();
    let (_tmp, cg, handler) = security_fixture();
    // Extremely large limit should still work (clamped to 100).
    let res = handler.execute(
        "codegraph_search",
        &json!({ "query": "example", "limit": 999999 }),
    );
    assert_ne!(res.is_error, Some(true));
    cg.close();
}

#[test]
fn rejects_non_string_symbol_in_codegraph_callers() {
    let _lock = env_read();
    let (_tmp, cg, handler) = security_fixture();
    let res = handler.execute("codegraph_callers", &json!({ "symbol": 123 }));
    assert_eq!(res.is_error, Some(true));
    assert!(first_text(&res).contains("non-empty string"));
    cg.close();
}

#[test]
fn rejects_non_string_query_in_codegraph_explore() {
    let _lock = env_read();
    let (_tmp, cg, handler) = security_fixture();
    // TS passes `query: undefined` — the key is absent on the wire.
    let res = handler.execute("codegraph_explore", &json!({}));
    assert_eq!(res.is_error, Some(true));
    assert!(first_text(&res).contains("non-empty string"));
    cg.close();
}

#[test]
fn truncates_oversized_tool_output_only_when_cap_env_is_set() {
    let _lock = env_write();
    // Server-side truncation is opt-in via CODEGRAPH_MAX_OUTPUT_CHARS; by
    // default the host owns inline-size policy and output is complete.
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let filler = "x".repeat(120);
    let mut content = String::new();
    for i in 0..150 {
        content.push_str(&format!(
            "export function trunc_{i}_{filler}() {{ return {i}; }}\n"
        ));
    }
    std::fs::write(src.join("trunc.ts"), content).unwrap();

    let cg = Rc::new(CodeGraph::init_sync(tmp.path()).unwrap());
    cg.index_all(&IndexOptions::default()).unwrap();
    let handler = ToolHandler::new(Some(Rc::clone(&cg)));
    let args = json!({ "query": "trunc", "limit": 999999 });

    // Default: complete output, no sentinel.
    let res = handler.execute("codegraph_search", &args);
    assert_ne!(res.is_error, Some(true));
    assert!(
        !first_text(&res).contains("... (output truncated)"),
        "expected complete output without a cap set"
    );

    // Opt-in cap: sentinel appears and size honors the cap.
    unsafe { std::env::set_var("CODEGRAPH_MAX_OUTPUT_CHARS", "15000") };
    let res = handler.execute("codegraph_search", &args);
    unsafe { std::env::remove_var("CODEGRAPH_MAX_OUTPUT_CHARS") };
    assert_ne!(res.is_error, Some(true));
    assert!(
        first_text(&res).contains("... (output truncated)"),
        "expected the truncation sentinel; got {} chars",
        first_text(&res).len()
    );
    cg.close();
}

#[test]
fn rejects_non_string_symbol_in_codegraph_impact() {
    let _lock = env_read();
    let (_tmp, cg, handler) = security_fixture();
    let res = handler.execute("codegraph_impact", &json!({ "symbol": [] }));
    assert_eq!(res.is_error, Some(true));
    cg.close();
}

#[test]
fn rejects_non_string_symbol_in_codegraph_node() {
    let _lock = env_read();
    let (_tmp, cg, handler) = security_fixture();
    let res = handler.execute("codegraph_node", &json!({ "symbol": false }));
    assert_eq!(res.is_error, Some(true));
    cg.close();
}

#[test]
fn rejects_non_string_symbol_in_codegraph_callees() {
    let _lock = env_read();
    let (_tmp, cg, handler) = security_fixture();
    let res = handler.execute("codegraph_callees", &json!({ "symbol": {} }));
    assert_eq!(res.is_error, Some(true));
    cg.close();
}

#[test]
fn handles_nan_limit_gracefully() {
    let _lock = env_read();
    let (_tmp, cg, handler) = security_fixture();
    let res = handler.execute(
        "codegraph_search",
        &json!({ "query": "example", "limit": "abc" }),
    );
    assert_ne!(res.is_error, Some(true));
    cg.close();
}

#[test]
fn handles_negative_limit_gracefully() {
    let _lock = env_read();
    let (_tmp, cg, handler) = security_fixture();
    let res = handler.execute(
        "codegraph_search",
        &json!({ "query": "example", "limit": -5 }),
    );
    assert_ne!(res.is_error, Some(true));
    cg.close();
}

// #230: getCodeGraph must reject a sensitive system directory passed as
// projectPath before opening it. The error surfaces through execute()'s
// catch as an isError result. /etc is sensitive on POSIX; C:\Windows on
// Windows (path resolution is platform-specific, so each case is gated).
#[cfg(unix)]
#[test]
fn rejects_a_sensitive_posix_project_path_via_the_mcp_handler() {
    let _lock = env_read();
    let (_tmp, cg, handler) = security_fixture();
    let res = handler.execute(
        "codegraph_search",
        &json!({ "query": "example", "projectPath": "/etc" }),
    );
    assert_eq!(res.is_error, Some(true));
    assert!(
        first_text(&res)
            .to_lowercase()
            .contains("sensitive system directory")
    );
    cg.close();
}

#[cfg(windows)]
#[test]
fn rejects_a_sensitive_windows_project_path_via_the_mcp_handler() {
    let _lock = env_read();
    let (_tmp, cg, handler) = security_fixture();
    let res = handler.execute(
        "codegraph_search",
        &json!({ "query": "example", "projectPath": "C:\\Windows" }),
    );
    assert_eq!(res.is_error, Some(true));
    assert!(
        first_text(&res)
            .to_lowercase()
            .contains("sensitive system directory")
    );
    cg.close();
}

//! CLI integration tests — port of `__tests__/status-json.test.ts` (the
//! CI/scripting fields `codegraph status --json` exposes, issue #329) plus an
//! end-to-end smoke of the clap CLI (init → index → query → callers →
//! affected → uninit) against the built binary.
//!
//! Like the TS suite, the CLI is exercised end-to-end against the real binary
//! (`CARGO_BIN_EXE_codegraph` ≙ `dist/bin/codegraph.js`) so the JSON field
//! names survive future refactors of the underlying plumbing. Real files,
//! real SQLite, no mocks.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use codegraph::{CodeGraph, IndexOptions};

const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_codegraph")
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// Run the built binary with `cwd`, stdin closed (no interactive prompts),
/// `CODEGRAPH_NO_DAEMON=1` pinned like the TS suite.
fn run_cli(cwd: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .stdin(Stdio::null())
        .output()
        .expect("spawn codegraph binary")
}

fn stdout_str(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// JSON mode prints exactly one line to stdout; be defensive about any stray
/// leading output by parsing the last non-empty line (TS `runStatusJson`).
fn run_status_json(cwd: &Path) -> serde_json::Value {
    let out = run_cli(cwd, &["status", "--json"]);
    assert!(
        out.status.success(),
        "status --json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = stdout_str(&out);
    let line = stdout
        .trim()
        .lines()
        .rfind(|l| !l.trim().is_empty())
        .expect("status --json printed nothing")
        .to_string();
    serde_json::from_str(&line).expect("status --json output is valid JSON")
}

/// Canonicalized tempdir (macOS /var → /private/var symlink parity).
fn temp_project() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix("codegraph-cli-test-")
        .tempdir()
        .expect("create tempdir");
    let root = dir.path().canonicalize().expect("canonicalize tempdir");
    (dir, root)
}

fn temp_project_without_parent_index() -> (tempfile::TempDir, PathBuf) {
    let mut candidates = vec![PathBuf::from("/var/tmp"), PathBuf::from("/dev/shm")];
    candidates.push(std::env::temp_dir());

    for base in candidates {
        if !base.is_dir() || has_codegraph_ancestor(&base) {
            continue;
        }
        let dir = tempfile::Builder::new()
            .prefix("codegraph-cli-test-")
            .tempdir_in(&base)
            .expect("create isolated tempdir");
        let root = dir.path().canonicalize().expect("canonicalize tempdir");
        return (dir, root);
    }

    temp_project()
}

fn has_codegraph_ancestor(path: &Path) -> bool {
    path.ancestors()
        .any(|ancestor| ancestor.join(".codegraph").join("codegraph.db").exists())
}

// =============================================================================
// ISO-8601 round-trip helpers (`new Date(ms).toISOString()` inverse)
// =============================================================================

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parse `YYYY-MM-DDTHH:MM:SS.mmmZ` back into epoch milliseconds
/// (`Date.parse` for the exact `toISOString` shape).
fn parse_iso_ms(s: &str) -> i64 {
    assert!(
        s.len() == 24 && s.ends_with('Z'),
        "unexpected ISO-8601 shape: {s}"
    );
    let num = |range: std::ops::Range<usize>| -> i64 {
        s[range.clone()]
            .parse()
            .unwrap_or_else(|_| panic!("non-numeric ISO segment in {s}"))
    };
    let days = days_from_civil(num(0..4), num(5..7), num(8..10));
    (days * 86_400 + num(11..13) * 3600 + num(14..16) * 60 + num(17..19)) * 1000 + num(20..23)
}

// =============================================================================
// status --json — CI fields (#329) — port of __tests__/status-json.test.ts
// =============================================================================

#[test]
fn get_last_indexed_at_is_null_before_indexing_and_a_recent_ms_timestamp_after() {
    let (_dir, root) = temp_project();

    let cg = CodeGraph::init_sync(&root).expect("init_sync");
    assert_eq!(cg.get_last_indexed_at().unwrap(), None);

    fs::write(root.join("a.ts"), "export const x = 1;\n").unwrap();
    let before = now_ms();
    let result = cg.index_all(&IndexOptions::default()).expect("index_all");
    assert!(result.success, "indexAll should succeed");
    let after = now_ms();

    let last = cg.get_last_indexed_at().unwrap();
    let last = last.expect("lastIndexedAt set after indexing");
    assert!(last >= before - 1000, "lastIndexedAt too early: {last}");
    assert!(last <= after + 1000, "lastIndexedAt too late: {last}");
    cg.close();
}

#[test]
fn status_json_on_an_uninitialized_project_reports_version_index_path_last_indexed_null() {
    let (_dir, root) = temp_project_without_parent_index();

    let out = run_status_json(&root);
    assert_eq!(out["initialized"], serde_json::json!(false));
    assert_eq!(out["version"], serde_json::json!(PKG_VERSION));
    let index_path = out["indexPath"].as_str().expect("indexPath is a string");
    assert!(index_path.contains(".codegraph"), "indexPath: {index_path}");
    assert!(out["lastIndexed"].is_null());
}

#[test]
fn status_json_on_an_indexed_project_reports_version_index_path_and_round_trippable_last_indexed() {
    let (_dir, root) = temp_project();
    fs::write(root.join("a.ts"), "export const x = 1;\n").unwrap();

    let before = now_ms();
    {
        let cg = CodeGraph::init_sync(&root).expect("init_sync");
        let result = cg.index_all(&IndexOptions::default()).expect("index_all");
        assert!(result.success);
        cg.close();
    }
    let after = now_ms();

    let out = run_status_json(&root);
    assert_eq!(out["initialized"], serde_json::json!(true));
    assert_eq!(out["version"], serde_json::json!(PKG_VERSION));
    assert!(
        out["indexPath"]
            .as_str()
            .expect("indexPath is a string")
            .contains(".codegraph")
    );
    let last_indexed = out["lastIndexed"]
        .as_str()
        .expect("lastIndexed is an ISO string");
    // ISO string that round-trips back into the index window.
    let ms = parse_iso_ms(last_indexed);
    assert!(ms >= before - 1000, "lastIndexed too early: {last_indexed}");
    assert!(ms <= after + 1000, "lastIndexed too late: {last_indexed}");
    // Wire-shape: backend reports "native", journal mode and pending changes
    // are present with the exact TS key names.
    assert_eq!(out["backend"], serde_json::json!("native"));
    assert!(out["journalMode"].is_string());
    assert!(out["pendingChanges"]["added"].is_number());
    assert!(out["pendingChanges"]["modified"].is_number());
    assert!(out["pendingChanges"]["removed"].is_number());
    assert!(out["nodesByKind"].is_object());
    assert!(out["languages"].is_array());
    assert!(out["worktreeMismatch"].is_null());
    assert!(out["fileCount"].as_u64().unwrap() >= 1);
    assert!(out["dbSizeBytes"].as_u64().unwrap() > 0);
}

// =============================================================================
// End-to-end smoke: init → index → query → callers → affected → uninit
// =============================================================================

fn write_smoke_fixture(root: &Path) {
    fs::create_dir_all(root.join("src/__tests__")).unwrap();
    fs::write(
        root.join("src/util.ts"),
        "export function add(a: number, b: number): number {\n  return a + b;\n}\n\nexport function double(n: number): number {\n  return add(n, n);\n}\n",
    )
    .unwrap();
    fs::write(
        root.join("src/__tests__/util.test.ts"),
        "import { add } from '../util';\nexport const result = add(1, 2);\n",
    )
    .unwrap();
}

#[test]
fn end_to_end_smoke_init_query_affected_uninit() {
    let (_dir, root) = temp_project_without_parent_index();
    write_smoke_fixture(&root);

    // --- init (builds the initial index by default) ---
    let out = run_cli(&root, &["init"]);
    assert!(
        out.status.success(),
        "init failed: stdout={} stderr={}",
        stdout_str(&out),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = stdout_str(&out);
    assert!(stdout.contains("Initialized in"), "init stdout: {stdout}");
    assert!(stdout.contains("Indexed"), "init stdout: {stdout}");
    assert!(root.join(".codegraph").join("codegraph.db").exists());

    // --- query --json ---
    let out = run_cli(&root, &["query", "add", "--json"]);
    assert!(out.status.success(), "query --json failed");
    let results: serde_json::Value =
        serde_json::from_str(stdout_str(&out).trim()).expect("query --json is valid JSON");
    let results = results.as_array().expect("query --json is an array");
    assert!(!results.is_empty(), "query found no results");
    assert!(
        results.iter().any(|r| r["node"]["name"] == "add"),
        "query results missing the `add` symbol: {results:?}"
    );

    // --- query (human output) ---
    let out = run_cli(&root, &["query", "add"]);
    assert!(out.status.success());
    assert!(stdout_str(&out).contains("Search Results for \"add\""));

    // --- callers --json (double() calls add() in the same file) ---
    let out = run_cli(&root, &["callers", "add", "--json"]);
    assert!(out.status.success(), "callers --json failed");
    let callers: serde_json::Value =
        serde_json::from_str(stdout_str(&out).trim()).expect("callers --json is valid JSON");
    assert_eq!(callers["symbol"], serde_json::json!("add"));
    let names: Vec<&str> = callers["callers"]
        .as_array()
        .expect("callers array")
        .iter()
        .filter_map(|c| c["name"].as_str())
        .collect();
    assert!(
        names.contains(&"double"),
        "callers of add should include double, got {names:?}"
    );

    // --- affected: a changed test file is itself reported ---
    let out = run_cli(
        &root,
        &["affected", "src/__tests__/util.test.ts", "--quiet"],
    );
    assert!(out.status.success(), "affected --quiet failed");
    assert!(stdout_str(&out).contains("src/__tests__/util.test.ts"));

    // --- affected --json on a plain source file (exit 0, exact key names) ---
    let out = run_cli(&root, &["affected", "src/util.ts", "--json"]);
    assert!(out.status.success(), "affected --json failed");
    let affected: serde_json::Value =
        serde_json::from_str(stdout_str(&out).trim()).expect("affected --json is valid JSON");
    assert_eq!(affected["changedFiles"], serde_json::json!(["src/util.ts"]));
    assert!(affected["affectedTests"].is_array());
    assert!(affected["totalDependentsTraversed"].is_number());

    // --- status --json sees both fixture files ---
    let status = run_status_json(&root);
    assert_eq!(status["initialized"], serde_json::json!(true));
    assert!(status["fileCount"].as_u64().unwrap() >= 2);

    // --- uninit -f removes .codegraph/ ---
    let out = run_cli(&root, &["uninit", "-f"]);
    assert!(
        out.status.success(),
        "uninit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout_str(&out).contains("Removed CodeGraph from"));
    assert!(!root.join(".codegraph").exists());

    // --- status --json reports uninitialized again ---
    let status = run_status_json(&root);
    assert_eq!(status["initialized"], serde_json::json!(false));
}

// =============================================================================
// unlock
// =============================================================================

#[test]
fn unlock_removes_a_stale_lock_file_and_is_a_noop_without_one() {
    let (_dir, root) = temp_project();
    {
        let cg = CodeGraph::init_sync(&root).expect("init_sync");
        cg.close();
    }

    let lock_path = root.join(".codegraph").join("codegraph.lock");
    fs::write(&lock_path, "999999").unwrap();

    let out = run_cli(&root, &["unlock"]);
    assert!(out.status.success());
    assert!(stdout_str(&out).contains("Removed lock file. You can now run indexing again."));
    assert!(!lock_path.exists());

    // Second run: nothing to do
    let out = run_cli(&root, &["unlock"]);
    assert!(out.status.success());
    assert!(stdout_str(&out).contains("No lock file found"));
}

// =============================================================================
// Help / version / parse-error exit codes
// =============================================================================

#[test]
fn help_lists_every_subcommand() {
    let (_dir, root) = temp_project();
    let out = run_cli(&root, &["--help"]);
    assert!(out.status.success(), "--help should exit 0");
    let stdout = stdout_str(&out);
    for sub in [
        "init",
        "uninit",
        "index",
        "sync",
        "status",
        "query",
        "files",
        "serve",
        "unlock",
        "callers",
        "callees",
        "impact",
        "affected",
        "install",
        "uninstall",
    ] {
        assert!(
            stdout.contains(sub),
            "--help missing subcommand {sub}:\n{stdout}"
        );
    }
}

#[test]
fn init_rejects_removed_index_flag() {
    let (_dir, root) = temp_project();
    let out = run_cli(&root, &["init", "-i"]);

    assert!(!out.status.success(), "init -i must be rejected");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unexpected argument '-i'") || stderr.contains("unexpected argument"),
        "stderr: {stderr}"
    );
}

#[test]
fn version_prints_the_bare_package_version() {
    let (_dir, root) = temp_project();
    let out = run_cli(&root, &["--version"]);
    assert!(out.status.success());
    // commander's `.version()` prints the bare string — byte parity.
    assert_eq!(stdout_str(&out), format!("{PKG_VERSION}\n"));
}

#[test]
fn unknown_command_exits_1_like_commander() {
    let (_dir, root) = temp_project();
    let out = run_cli(&root, &["definitely-not-a-command"]);
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn status_human_output_reports_not_initialized() {
    let (_dir, root) = temp_project_without_parent_index();
    let out = run_cli(&root, &["status"]);
    assert!(out.status.success());
    let stdout = stdout_str(&out);
    assert!(stdout.contains("CodeGraph Status"), "stdout: {stdout}");
    assert!(stdout.contains("Not initialized"), "stdout: {stdout}");
    assert!(
        stdout.contains("Run \"codegraph init\" to initialize"),
        "stdout: {stdout}"
    );
}

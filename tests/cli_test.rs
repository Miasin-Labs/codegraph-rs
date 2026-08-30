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

fn run_cli_with_home(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("HOME", home)
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

#[tokio::test(flavor = "current_thread")]
async fn get_last_indexed_at_is_null_before_indexing_and_a_recent_ms_timestamp_after() {
    let (_dir, root) = temp_project();

    let cg = CodeGraph::init_sync(&root).expect("init_sync");
    assert_eq!(cg.get_last_indexed_at().unwrap(), None);

    fs::write(root.join("a.ts"), "export const x = 1;\n").unwrap();
    let before = now_ms();
    let result = cg
        .index_all(&IndexOptions::default())
        .await
        .expect("index_all");
    assert!(result.success, "indexAll should succeed");
    let after = now_ms();

    let last = cg.get_last_indexed_at().unwrap();
    let last = last.expect("lastIndexedAt set after indexing");
    assert!(last >= before - 1000, "lastIndexedAt too early: {last}");
    assert!(last <= after + 1000, "lastIndexedAt too late: {last}");
    cg.close();
}

#[tokio::test(flavor = "current_thread")]
async fn status_json_on_an_uninitialized_project_reports_version_index_path_last_indexed_null() {
    let (_dir, root) = temp_project_without_parent_index();

    let out = run_status_json(&root);
    assert_eq!(out["initialized"], serde_json::json!(false));
    assert_eq!(out["version"], serde_json::json!(PKG_VERSION));
    let index_path = out["indexPath"].as_str().expect("indexPath is a string");
    assert!(index_path.contains(".codegraph"), "indexPath: {index_path}");
    assert!(out["lastIndexed"].is_null());
}

#[tokio::test(flavor = "current_thread")]
async fn status_json_on_an_indexed_project_reports_version_index_path_and_round_trippable_last_indexed()
 {
    let (_dir, root) = temp_project();
    fs::write(root.join("a.ts"), "// @generated\nexport const x = 1;\n").unwrap();

    let before = now_ms();
    {
        let cg = CodeGraph::init_sync(&root).expect("init_sync");
        let result = cg
            .index_all(&IndexOptions::default())
            .await
            .expect("index_all");
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
    assert_eq!(out["generatedFileCount"], serde_json::json!(1));
    assert!(out["nodesByKind"].is_object());
    assert!(out["languages"].is_array());
    assert!(out["worktreeMismatch"].is_null());
    assert!(out["fileCount"].as_u64().unwrap() >= 1);
    assert!(out["dbSizeBytes"].as_u64().unwrap() > 0);
}

#[tokio::test(flavor = "current_thread")]
async fn status_json_preserves_exact_pending_shape_while_human_output_lists_paths() {
    let (_dir, root) = temp_project();
    fs::write(root.join("a.ts"), "export const a = 1;\n").unwrap();
    fs::write(root.join("c.ts"), "export const c = 1;\n").unwrap();
    {
        let cg = CodeGraph::init_sync(&root).expect("init_sync");
        let result = cg
            .index_all(&IndexOptions::default())
            .await
            .expect("index_all");
        assert!(result.success);
        cg.close();
    }

    fs::write(root.join("a.ts"), "export const a = 2;\n").unwrap();
    fs::write(root.join("b.ts"), "export const b = 1;\n").unwrap();
    fs::remove_file(root.join("c.ts")).unwrap();

    let out = run_status_json(&root);
    assert_eq!(
        out["pendingChanges"],
        serde_json::json!({
            "added": 1,
            "modified": 1,
            "removed": 1,
        })
    );

    let human = run_cli(&root, &["status"]);
    assert!(
        human.status.success(),
        "{}",
        String::from_utf8_lossy(&human.stderr)
    );
    let human = stdout_str(&human);
    assert!(human.contains("b.ts"), "{human}");
    assert!(human.contains("a.ts"), "{human}");
    assert!(human.contains("c.ts"), "{human}");
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

#[tokio::test(flavor = "current_thread")]
async fn end_to_end_smoke_init_query_affected_uninit() {
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
    assert_eq!(
        affected["affectedTests"],
        serde_json::json!(["src/__tests__/util.test.ts"])
    );
    assert!(affected["totalDependentsTraversed"].is_number());

    let absolute_util = root.join("src/util.ts").to_string_lossy().to_string();
    let out = run_cli(&root, &["affected", &absolute_util, "--json"]);
    assert!(out.status.success(), "affected absolute path failed");
    let affected: serde_json::Value =
        serde_json::from_str(stdout_str(&out).trim()).expect("affected --json is valid JSON");
    assert_eq!(affected["changedFiles"], serde_json::json!(["src/util.ts"]));
    assert_eq!(
        affected["affectedTests"],
        serde_json::json!(["src/__tests__/util.test.ts"])
    );

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
    assert!(!root.join(".codegraph").join("codegraph.db").exists());

    // --- status --json reports uninitialized again ---
    let status = run_status_json(&root);
    assert_eq!(status["initialized"], serde_json::json!(false));
}

#[test]
fn init_refuses_the_effective_home_directory_without_force() {
    // Given: an existing directory presented to the CLI as HOME.
    let (_dir, root) = temp_project_without_parent_index();
    fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();

    // When: initialization targets that directory without an override.
    let out = run_cli_with_home(&root, &root, &["init"]);

    // Then: the command refuses before creating an index.
    assert!(!out.status.success());
    assert!(!root.join(".codegraph").join("codegraph.db").exists());
    let output = format!(
        "{}{}",
        stdout_str(&out),
        String::from_utf8_lossy(&out.stderr)
    )
    .to_lowercase();
    assert!(output.contains("home directory"), "{output}");
    assert!(output.contains("--force"), "{output}");
}

#[test]
fn init_force_explicitly_allows_the_effective_home_directory() {
    // Given: an existing directory presented to the CLI as HOME.
    let (_dir, root) = temp_project_without_parent_index();
    fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();

    // When: the caller explicitly accepts the broad indexing scope.
    let out = run_cli_with_home(&root, &root, &["init", "--force"]);

    // Then: initialization succeeds and creates the index.
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        stdout_str(&out),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(root.join(".codegraph").join("codegraph.db").exists());
}

// =============================================================================
// unlock
// =============================================================================

#[tokio::test(flavor = "current_thread")]
async fn unlock_removes_a_stale_lock_file_and_is_a_noop_without_one() {
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

#[tokio::test(flavor = "current_thread")]
async fn help_lists_every_subcommand() {
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

#[tokio::test(flavor = "current_thread")]
async fn init_rejects_removed_index_flag() {
    let (_dir, root) = temp_project();
    let out = run_cli(&root, &["init", "-i"]);

    assert!(!out.status.success(), "init -i must be rejected");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unexpected argument '-i'") || stderr.contains("unexpected argument"),
        "stderr: {stderr}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn version_prints_the_bare_package_version() {
    let (_dir, root) = temp_project();
    let out = run_cli(&root, &["--version"]);
    assert!(out.status.success());
    // commander's `.version()` prints the bare string — byte parity.
    assert_eq!(stdout_str(&out), format!("{PKG_VERSION}\n"));
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_command_exits_1_like_commander() {
    let (_dir, root) = temp_project();
    let out = run_cli(&root, &["definitely-not-a-command"]);
    assert_eq!(out.status.code(), Some(1));
}

#[tokio::test(flavor = "current_thread")]
async fn status_human_output_reports_not_initialized() {
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

// =============================================================================
// #1512: callers/callees/impact must attribute edges per distinct definition
// and accept --file to scope. Before the fix, every same-named definition was
// merged into one flat, unlabeled result and there was no --file option, so the
// answer was a union true of no single definition.
// =============================================================================

fn write_multi_handle_fixture(root: &Path) {
    // Two `handle` definitions in different directories: each calls a different
    // helper and is called by a different caller. Mirrors the issue's repro.
    for (dir, helper, caller_fn, caller_name) in [
        ("a", "alpha", "aMain", "aMain"),
        ("b", "beta", "bMain", "bMain"),
    ] {
        fs::create_dir_all(root.join(dir)).unwrap();
        fs::write(
            root.join(dir).join(format!("{helper}.js")),
            format!("function {helper}() {{ return 1; }}\nmodule.exports = {{ {helper} }};\n"),
        )
        .unwrap();
        fs::write(
            root.join(dir).join("svc.js"),
            format!(
                "const {{ {helper} }} = require('./{helper}');\nfunction handle() {{ return {helper}(); }}\nmodule.exports = {{ handle }};\n"
            ),
        )
        .unwrap();
        fs::write(
            root.join(dir).join("main.js"),
            format!(
                "const {{ handle }} = require('./svc');\nfunction {caller_fn}() {{ return handle(); }}\nmodule.exports = {{ {caller_name} }};\n"
            ),
        )
        .unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn callers_callees_impact_attribute_per_definition_and_scope_by_file() {
    let (_dir, root) = temp_project_without_parent_index();
    write_multi_handle_fixture(&root);

    let out = run_cli(&root, &["init"]);
    assert!(
        out.status.success(),
        "init failed: stdout={} stderr={}",
        stdout_str(&out),
        String::from_utf8_lossy(&out.stderr)
    );

    // --- callees --json: two distinct definitions, each with its own edges ---
    let out = run_cli(&root, &["callees", "handle", "--json"]);
    assert!(out.status.success(), "callees --json failed");
    let v: serde_json::Value =
        serde_json::from_str(stdout_str(&out).trim()).expect("callees --json is valid JSON");
    let defs = v["definitions"]
        .as_array()
        .expect("multi-definition output nests a `definitions` array");
    assert_eq!(defs.len(), 2, "two `handle` definitions expected: {v}");
    // No single definition may claim both alpha and beta — that was the merge bug.
    for def in defs {
        let names: Vec<&str> = def["callees"]
            .as_array()
            .expect("callees array")
            .iter()
            .filter_map(|c| c["name"].as_str())
            .collect();
        let has_alpha = names.contains(&"alpha");
        let has_beta = names.contains(&"beta");
        assert!(
            has_alpha ^ has_beta,
            "a single handle definition must call exactly one of alpha/beta, got {names:?}"
        );
        // The definition carries its own file attribution.
        assert!(
            def["definition"]["filePath"]
                .as_str()
                .unwrap()
                .contains("svc.js"),
            "definition should be attributed to its file: {def}"
        );
    }

    // --- callees --file a/svc.js: scopes to the one definition ---
    let out = run_cli(
        &root,
        &["callees", "handle", "--file", "a/svc.js", "--json"],
    );
    assert!(out.status.success(), "callees --file failed");
    let v: serde_json::Value =
        serde_json::from_str(stdout_str(&out).trim()).expect("scoped callees is valid JSON");
    assert!(
        v.get("definitions").is_none(),
        "a single scoped definition uses the flat envelope: {v}"
    );
    let names: Vec<&str> = v["callees"]
        .as_array()
        .expect("callees array")
        .iter()
        .filter_map(|c| c["name"].as_str())
        .collect();
    assert!(
        names.contains(&"alpha") && !names.contains(&"beta"),
        "a/svc.js handle calls only alpha, got {names:?}"
    );

    // --- callers --json: attributed per definition, never merged ---
    let out = run_cli(&root, &["callers", "handle", "--json"]);
    assert!(out.status.success(), "callers --json failed");
    let v: serde_json::Value =
        serde_json::from_str(stdout_str(&out).trim()).expect("callers --json is valid JSON");
    let defs = v["definitions"].as_array().expect("`definitions` array");
    assert_eq!(defs.len(), 2, "two `handle` definitions expected: {v}");
    for def in defs {
        let names: Vec<&str> = def["callers"]
            .as_array()
            .expect("callers array")
            .iter()
            .filter_map(|c| c["name"].as_str())
            .collect();
        let has_a = names.contains(&"aMain");
        let has_b = names.contains(&"bMain");
        assert!(
            has_a ^ has_b,
            "a single handle definition must be called by exactly one of aMain/bMain, got {names:?}"
        );
    }

    // --- impact --json: one blast radius per definition ---
    let out = run_cli(&root, &["impact", "handle", "--json"]);
    assert!(out.status.success(), "impact --json failed");
    let v: serde_json::Value =
        serde_json::from_str(stdout_str(&out).trim()).expect("impact --json is valid JSON");
    let defs = v["definitions"].as_array().expect("`definitions` array");
    assert_eq!(
        defs.len(),
        2,
        "two `handle` impact definitions expected: {v}"
    );

    // --- --file matching nothing: keep all defs and report it via `note` ---
    let out = run_cli(
        &root,
        &["callees", "handle", "--file", "does/not/exist.js", "--json"],
    );
    assert!(out.status.success(), "callees --file miss failed");
    let v: serde_json::Value = serde_json::from_str(stdout_str(&out).trim()).expect("valid JSON");
    assert!(
        v["note"].as_str().unwrap_or("").contains("no definition"),
        "a non-matching --file must be reported, not ignored: {v}"
    );
    assert_eq!(
        v["definitions"].as_array().map(|d| d.len()),
        Some(2),
        "a missed --file falls back to showing all definitions: {v}"
    );
}

// index <path> must not silently rebuild an ancestor's index (issue #1524)
// =============================================================================

#[test]
fn index_explicit_uninitialized_path_refuses_instead_of_indexing_ancestor() {
    // Given: only the parent is initialized; its child is an explicit, un-indexed
    // subdirectory. `index child/` used to walk up, rebuild the PARENT index, and
    // print success — never saying the named child had been ignored (#1524).
    let (_dir, root) = temp_project_without_parent_index();
    let parent = root.join("parent");
    let child = parent.join("child");
    fs::create_dir_all(&child).unwrap();
    fs::write(parent.join("p.py"), "def parent_only():\n    return 1\n").unwrap();
    fs::write(child.join("c.py"), "def child_only():\n    return 2\n").unwrap();

    let init = run_cli(&parent, &["init", "."]);
    assert!(
        init.status.success(),
        "init parent failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    // When: the child is named explicitly for indexing.
    let out = run_cli(&child, &["index", "."]);

    // Then: it refuses (non-zero) and names the child path — no silent success,
    // and the child is never given an index of the ancestor's contents.
    assert!(
        !out.status.success(),
        "index of an uninitialized explicit path should fail, not silently index an ancestor"
    );
    assert!(
        !child.join(".codegraph").join("codegraph.db").exists(),
        "child must not gain an index"
    );
    let combined = format!(
        "{}{}",
        stdout_str(&out),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("not initialized"),
        "message should explain the child is not initialized: {combined}"
    );
    assert!(
        combined.contains("child"),
        "message should reference the named path, not an ancestor: {combined}"
    );
    assert!(
        !combined.contains("Indexed"),
        "must not report a successful index: {combined}"
    );
}

#[test]
fn bare_index_from_subdirectory_still_resolves_initialized_ancestor() {
    // A bare `index` (no path) keeps the query-style convenience of resolving the
    // nearest initialized project, so running it from a subdir of an initialized
    // project still indexes that project. Only an EXPLICIT path is literal (#1524).
    let (_dir, root) = temp_project_without_parent_index();
    let sub = root.join("sub");
    fs::create_dir_all(&sub).unwrap();
    fs::write(root.join("main.py"), "def top():\n    return 1\n").unwrap();

    let init = run_cli(&root, &["init", "."]);
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let out = run_cli(&sub, &["index"]);
    assert!(
        out.status.success(),
        "bare index from a subdir of an initialized project should succeed: {} {}",
        stdout_str(&out),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout_str(&out).contains("Indexed"),
        "bare index should report indexing the ancestor project: {}",
        stdout_str(&out)
    );
}

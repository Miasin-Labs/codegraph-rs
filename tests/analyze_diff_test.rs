//! CLI integration tests for `codegraph analyze diff` — working-tree vs
//! base comparison over the snapshot cache (`.codegraph/analysis/`, one
//! rotated `.prev` generation).
//!
//! Like `analyze_cache_test.rs`, the CLI is exercised end-to-end against the
//! built binary (`CARGO_BIN_EXE_codegraph`): real files, real SQLite, real
//! cache files, no mocks. The diff contract under test:
//!
//! - index fixture → any analyze command caches the base → modify a file +
//!   re-index → `analyze diff` shows exactly the changed function (position
//!   shifts of untouched neighbors are NOT changes),
//! - no base snapshot → exit 0 with the honest note (and the run primes the
//!   cache so the next edit/diff works),
//! - the diff run annotates the current snapshot with per-function
//!   complexity, so the NEXT diff reports full before/after deltas,
//! - newly-introduced SCC cycles are surfaced (pre-existing ones are not),
//! - `--base <path>` loads an explicit snapshot file or cache directory.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_codegraph")
}

/// Run the built binary with `cwd`, stdin closed, `CODEGRAPH_NO_DAEMON=1`
/// pinned like the rest of the CLI suite, and any ambient cache-dir
/// override stripped (these tests reason about exact cache locations).
fn run_cli(cwd: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env_remove("CODEGRAPH_ANALYSIS_CACHE_DIR")
        .stdin(Stdio::null())
        .output()
        .expect("spawn codegraph binary")
}

fn stdout_str(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr_str(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// Run an analyze subcommand with `--json` and parse the versioned envelope.
fn run_analyze_json(cwd: &Path, args: &[&str]) -> serde_json::Value {
    let mut full: Vec<&str> = vec!["analyze"];
    full.extend_from_slice(args);
    full.push("--json");
    let out = run_cli(cwd, &full);
    assert!(
        out.status.success(),
        "analyze {} failed: {}",
        args.join(" "),
        stderr_str(&out)
    );
    serde_json::from_str(stdout_str(&out).trim()).unwrap_or_else(|e| {
        panic!(
            "analyze {} did not print valid JSON ({e}): {}",
            args.join(" "),
            stdout_str(&out)
        )
    })
}

/// Canonicalized tempdir (macOS /var → /private/var symlink parity).
fn temp_project() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix("codegraph-analyze-diff-test-")
        .tempdir()
        .expect("create tempdir");
    let root = dir.path().canonicalize().expect("canonicalize tempdir");
    (dir, root)
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

/// Same ground-truth fixture as `analyze_cli_test.rs`/`analyze_cache_test.rs`:
/// the call chain `main → compute → helper` and the mutual-recursion pair
/// `ping ↔ pong`. `compute` sits ABOVE ping/pong in util.ts so that growing
/// its body shifts their positions — the diff must not flag the shift.
fn write_fixture(root: &Path) {
    write(
        &root.join("src/util.ts"),
        r#"export function helper(x: number): number {
  if (x > 3) {
    return x * 2;
  }
  return x + 1;
}

export function compute(x: number): number {
  let total = 0;
  for (let i = 0; i < x; i++) {
    if (i % 2 === 0) {
      total += helper(i);
    } else {
      total -= 1;
    }
  }
  return total;
}

export function ping(n: number): number {
  return n <= 0 ? 0 : pong(n - 1);
}

export function pong(n: number): number {
  return n <= 0 ? 1 : ping(n - 1);
}
"#,
    );
    write(
        &root.join("src/main.ts"),
        r#"import { compute } from './util';

export function main(): number {
  return compute(10);
}
"#,
    );
}

fn init_fixture(root: &Path) {
    write_fixture(root);
    let out = run_cli(root, &["init"]);
    assert!(out.status.success(), "init failed: {}", stderr_str(&out));
}

fn reindex(root: &Path) {
    let out = run_cli(root, &["index", "--force", "--quiet"]);
    assert!(
        out.status.success(),
        "re-index failed: {}",
        stderr_str(&out)
    );
}

/// Grow `compute`'s body by one guard branch (changes its span/byte length
/// and cyclomatic complexity; everything below it in the file only shifts).
fn grow_compute(root: &Path, guard: &str) {
    let util = root.join("src/util.ts");
    let source = fs::read_to_string(&util).unwrap();
    let needle = "  let total = 0;\n";
    assert!(source.contains(needle), "fixture anchor present");
    let replacement = format!("  let total = 0;\n  if (x {guard}) {{\n    return -1;\n  }}\n");
    fs::write(&util, source.replacen(needle, &replacement, 1)).unwrap();
}

fn changed_function_names(data: &serde_json::Value) -> Vec<String> {
    data["changedFunctions"]
        .as_array()
        .expect("changedFunctions array")
        .iter()
        .map(|f| f["symbol"]["name"].as_str().unwrap().to_string())
        .collect()
}

const NO_BASE_NOTE: &str = "no base snapshot — run any analyze command on the base state first";

// =============================================================================
// the canonical flow: analyze → edit → re-index → diff
// =============================================================================

#[test]
fn diff_shows_exactly_the_changed_function_and_its_impact() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    // Any analyze command caches the base snapshot.
    let _ = run_analyze_json(&root, &["cycles"]);

    grow_compute(&root, "< 0");
    reindex(&root);

    let envelope = run_analyze_json(&root, &["diff"]);
    assert_eq!(envelope["kind"], "diff");
    let data = &envelope["data"];

    // Exactly one changed function: compute. helper (above the edit),
    // ping/pong (below it — pure position shift), and main are untouched.
    assert_eq!(changed_function_names(data), vec!["compute"]);
    assert_eq!(data["nodesAddedCount"].as_u64(), Some(0));
    assert_eq!(data["nodesRemovedCount"].as_u64(), Some(0));
    let changed_names: Vec<&str> = data["nodesChanged"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["symbol"]["name"].as_str().unwrap())
        .collect();
    assert!(changed_names.contains(&"compute"), "{changed_names:?}");
    for untouched in ["helper", "ping", "pong", "main"] {
        assert!(
            !changed_names.contains(&untouched),
            "{untouched} only shifted position and must not be flagged: {changed_names:?}"
        );
    }

    // The base generation was rotated to .prev when the diff's own bridge
    // refreshed the cache to the working tree's fingerprint.
    assert_eq!(data["base"]["source"].as_str(), Some("cache-prev"));
    assert!(data["base"]["indexFingerprint"].is_string());

    // Working-tree complexity is measured; base complexity is honestly
    // absent (the base snapshot predates any diff run).
    let delta = &data["changedFunctions"][0];
    assert!(delta["cyclomaticAfter"].as_u64().is_some());
    assert!(delta["cyclomaticBefore"].is_null());
    assert!(delta["cyclomaticDelta"].is_null());
    assert!(
        delta["linesAfter"].as_u64() > delta["linesBefore"].as_u64(),
        "compute grew: {delta}"
    );
    assert!(
        data["note"]
            .as_str()
            .unwrap()
            .contains("Base complexity is unavailable")
    );

    // Impact of the delta: compute's caller.
    let impacted: Vec<&str> = data["impact"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["name"].as_str().unwrap())
        .collect();
    assert!(impacted.contains(&"main"), "{impacted:?}");

    // Human output names the changed function and stays exit-0.
    let out = run_cli(&root, &["analyze", "diff"]);
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    let human = stdout_str(&out);
    assert!(human.contains("compute"), "{human}");
    assert!(human.contains("Diff vs base snapshot"), "{human}");
}

// =============================================================================
// no-base case
// =============================================================================

#[test]
fn no_base_case_exits_zero_with_the_honest_note_and_primes_the_cache() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    // No analyze command has run → no snapshot older than the working tree.
    let out = run_cli(&root, &["analyze", "diff"]);
    assert!(out.status.success(), "no-base diff must exit 0");
    assert!(
        stdout_str(&out).contains(NO_BASE_NOTE),
        "honest note expected: {}",
        stdout_str(&out)
    );

    // JSON form is equally honest (and still exit 0).
    let envelope = run_analyze_json(&root, &["diff"]);
    assert_eq!(envelope["data"]["baseAvailable"].as_bool(), Some(false));
    assert_eq!(envelope["data"]["note"].as_str(), Some(NO_BASE_NOTE));

    // Re-running diff with an unchanged index still has nothing older to
    // compare against — same note, not an empty fake diff.
    let out = run_cli(&root, &["analyze", "diff"]);
    assert!(out.status.success());
    assert!(stdout_str(&out).contains(NO_BASE_NOTE));

    // But the no-base run primed the snapshot cache: edit → re-index →
    // diff now works without any other analyze command in between.
    grow_compute(&root, "< 0");
    reindex(&root);
    let envelope = run_analyze_json(&root, &["diff"]);
    assert_eq!(changed_function_names(&envelope["data"]), vec!["compute"]);
}

// =============================================================================
// complexity deltas from the annotated base (second-generation diff)
// =============================================================================

#[test]
fn second_diff_reports_full_complexity_deltas_from_the_annotated_base() {
    let (_dir, root) = temp_project();
    init_fixture(&root);
    let _ = run_analyze_json(&root, &["cycles"]);

    // First edit + diff: measures the working tree and annotates the
    // current snapshot generation with per-function complexity.
    grow_compute(&root, "< 0");
    reindex(&root);
    let first = run_analyze_json(&root, &["diff"]);
    let first_delta = &first["data"]["changedFunctions"][0];
    assert!(first_delta["cyclomaticBefore"].is_null());
    let after_first = first_delta["cyclomaticAfter"].as_u64().unwrap();

    // Second edit + diff: the base is now the annotated generation, so the
    // delta has both ends.
    grow_compute(&root, "> 100");
    reindex(&root);
    let second = run_analyze_json(&root, &["diff"]);
    let data = &second["data"];
    assert_eq!(changed_function_names(data), vec!["compute"]);
    let delta = &data["changedFunctions"][0];
    assert_eq!(delta["cyclomaticBefore"].as_u64(), Some(after_first));
    assert_eq!(
        delta["cyclomaticAfter"].as_u64(),
        Some(after_first + 1),
        "one extra guard branch: {delta}"
    );
    assert_eq!(delta["cyclomaticDelta"].as_i64(), Some(1));
    assert!(delta["cognitiveDelta"].as_i64().is_some());
    assert!(
        !data["note"]
            .as_str()
            .unwrap()
            .contains("Base complexity is unavailable")
    );

    // Repeating the diff with an unchanged index is stable: the base is the
    // rotated .prev generation, same answer.
    let repeat = run_analyze_json(&root, &["diff"]);
    assert_eq!(
        repeat["data"]["changedFunctions"], second["data"]["changedFunctions"],
        "diff is idempotent while the index is unchanged"
    );
    assert_eq!(repeat["data"]["base"], second["data"]["base"]);
}

// =============================================================================
// newly-introduced cycles + added nodes/edges
// =============================================================================

#[test]
fn diff_surfaces_new_cycles_and_added_symbols_only() {
    let (_dir, root) = temp_project();
    init_fixture(&root);
    let _ = run_analyze_json(&root, &["cycles"]);

    // Add a second mutual-recursion pair; ping/pong already exists in the
    // base and must NOT be reported as new.
    let util = root.join("src/util.ts");
    let mut source = fs::read_to_string(&util).unwrap();
    source.push_str(
        r#"
export function tick(n: number): number {
  return n <= 0 ? 0 : tock(n - 1);
}

export function tock(n: number): number {
  return n <= 0 ? 1 : tick(n - 1);
}
"#,
    );
    fs::write(&util, source).unwrap();
    reindex(&root);

    let envelope = run_analyze_json(&root, &["diff"]);
    let data = &envelope["data"];

    let added: Vec<&str> = data["nodesAdded"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["name"].as_str().unwrap())
        .collect();
    assert!(
        added.contains(&"tick") && added.contains(&"tock"),
        "{added:?}"
    );

    assert_eq!(data["newCycleCount"].as_u64(), Some(1), "{data}");
    let members: Vec<&str> = data["newCycles"][0]["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert_eq!(members, vec!["tick", "tock"]);
    assert_eq!(data["resolvedCycleCount"].as_u64(), Some(0));

    // The new call edges surfaced too.
    let edge_pairs: Vec<(String, String)> = data["edgesAdded"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["from"].as_str().unwrap().to_string(),
                e["to"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert!(
        edge_pairs
            .iter()
            .any(|(f, t)| f.contains("tick") && t.contains("tock")),
        "{edge_pairs:?}"
    );
}

// =============================================================================
// explicit --base <path>
// =============================================================================

#[test]
fn explicit_base_accepts_a_cache_directory_and_a_bare_snapshot_file() {
    let (_dir, root) = temp_project();
    init_fixture(&root);
    let _ = run_analyze_json(&root, &["cycles"]);

    // Preserve the base generation outside the cache before it rotates.
    let cache_dir = root.join(".codegraph").join("analysis");
    let kept = root.join("kept-base");
    fs::create_dir_all(&kept).unwrap();
    for file in ["graph.bin", "meta.json"] {
        fs::copy(cache_dir.join(file), kept.join(file)).unwrap();
    }

    grow_compute(&root, "< 0");
    reindex(&root);

    // Directory form (graph.bin + meta.json → fingerprint known).
    let envelope = run_analyze_json(&root, &["diff", "--base", kept.to_str().unwrap()]);
    let data = &envelope["data"];
    assert_eq!(data["base"]["source"].as_str(), Some("file"));
    assert!(data["base"]["indexFingerprint"].is_string());
    assert_eq!(changed_function_names(data), vec!["compute"]);

    // Bare snapshot-file form (no meta → fingerprint unknown, still diffs).
    let envelope = run_analyze_json(
        &root,
        &["diff", "--base", kept.join("graph.bin").to_str().unwrap()],
    );
    let data = &envelope["data"];
    assert_eq!(data["base"]["source"].as_str(), Some("file"));
    assert!(data["base"]["indexFingerprint"].is_null());
    assert_eq!(changed_function_names(data), vec!["compute"]);

    // A bogus explicit base is a real error (exit 1), not a silent empty.
    let out = run_cli(
        &root,
        &["analyze", "diff", "--base", "/nonexistent/snap.bin"],
    );
    assert!(!out.status.success());
    assert!(
        stderr_str(&out).contains("cannot load base snapshot"),
        "stderr: {}",
        stderr_str(&out)
    );
}

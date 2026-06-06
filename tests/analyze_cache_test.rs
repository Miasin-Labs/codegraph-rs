//! CLI integration tests for the `codegraph analyze` snapshot cache —
//! the bridged analysis graph persisted under `.codegraph/analysis/` and
//! keyed by an index fingerprint.
//!
//! Like `analyze_cli_test.rs`, the CLI is exercised end-to-end against the
//! built binary (`CARGO_BIN_EXE_codegraph`): real files, real SQLite, real
//! cache files on disk, no mocks. The cache contract under test:
//!
//! - first `analyze` run materializes `graph.bin` + `meta.json`,
//! - the second run serves the snapshot (one-line "(cached graph)" notice
//!   in human output; `--json` stays pure JSON) with identical output,
//! - a re-index that changes the store invalidates the snapshot,
//! - `--no-cache` bypasses the read path and rebuilds,
//! - corrupt cache files degrade to a silent rebuild (then self-heal),
//! - `CODEGRAPH_ANALYSIS_CACHE_DIR` relocates the cache out of the project.
//!
//! Wall-clock speedup is deliberately NOT asserted: at this fixture's scale
//! (5 functions) both the rebuild and the snapshot load are microseconds and
//! process spawn dominates, so a timing assertion would only measure OS
//! scheduler noise. The "(cached graph)" notice is printed exactly when the
//! snapshot loaded successfully, so it is the non-flaky witness that the SQL
//! re-read was skipped; the corruption test pins the fallback branch.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_codegraph")
}

/// Run the built binary with `cwd`, stdin closed (no interactive prompts),
/// `CODEGRAPH_NO_DAEMON=1` pinned like the rest of the CLI suite, and any
/// extra environment variables applied.
fn run_cli_env(cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(bin());
    cmd.args(args)
        .current_dir(cwd)
        .env("CODEGRAPH_NO_DAEMON", "1")
        // The cache tests reason about exact cache locations — never let an
        // ambient override from the developer's shell leak in.
        .env_remove("CODEGRAPH_ANALYSIS_CACHE_DIR")
        .stdin(Stdio::null());
    for (key, value) in envs {
        cmd.env(key, value);
    }
    cmd.output().expect("spawn codegraph binary")
}

fn run_cli(cwd: &Path, args: &[&str]) -> Output {
    run_cli_env(cwd, args, &[])
}

fn stdout_str(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr_str(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// Run an analyze subcommand with `--json` and parse stdout.
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
        .prefix("codegraph-analyze-cache-test-")
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

/// Same ground-truth fixture as `analyze_cli_test.rs`: the call chain
/// `main → compute → helper` and the mutual-recursion pair `ping ↔ pong`.
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

/// `codegraph init` builds the index by default; assert it worked.
fn init_fixture(root: &Path) {
    write_fixture(root);
    let out = run_cli(root, &["init"]);
    assert!(out.status.success(), "init failed: {}", stderr_str(&out));
}

fn cache_dir(root: &Path) -> PathBuf {
    root.join(".codegraph").join("analysis")
}

fn assert_cache_files_exist(dir: &Path) {
    assert!(
        dir.join("graph.bin").exists(),
        "graph snapshot missing in {dir:?}"
    );
    assert!(
        dir.join("meta.json").exists(),
        "cache meta missing in {dir:?}"
    );
}

const CACHE_NOTICE: &str = "(cached graph)";

// =============================================================================
// cache creation + hit
// =============================================================================

#[test]
fn first_run_creates_cache_and_second_run_hits_with_identical_output() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    // No cache before the first analyze run.
    assert!(!cache_dir(&root).exists(), "no cache before first analyze");

    // First run (miss): builds from SQL and materializes the snapshot.
    // `slice` output is fully sorted by the report, so it is byte-stable
    // across processes and a safe equality target.
    let first = run_analyze_json(&root, &["slice", "main"]);
    assert_eq!(first["size"].as_u64(), Some(2));
    assert_cache_files_exist(&cache_dir(&root));

    // Second run, human output: served from the snapshot, says so once.
    let out = run_cli(&root, &["analyze", "slice", "main"]);
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    let human = stdout_str(&out);
    assert!(
        human.contains(CACHE_NOTICE),
        "cache hit must print the notice in human output: {human}"
    );
    assert_eq!(
        human.matches(CACHE_NOTICE).count(),
        1,
        "the notice is one line, printed once"
    );

    // Third run, --json: pure JSON (no notice anywhere) and identical to
    // the rebuilt output.
    let out = run_cli(&root, &["analyze", "slice", "main", "--json"]);
    assert!(out.status.success());
    let json_stdout = stdout_str(&out);
    assert!(
        !json_stdout.contains(CACHE_NOTICE),
        "--json output must stay machine-pure on cache hits"
    );
    assert!(
        json_stdout.trim_start().starts_with('{'),
        "--json stdout starts with the JSON object"
    );
    let third: serde_json::Value = serde_json::from_str(json_stdout.trim()).unwrap();
    assert_eq!(
        first, third,
        "cached graph must reproduce the rebuilt output"
    );

    // The cache is shared across the whole analyze family: a different
    // subcommand also hits it.
    let out = run_cli(&root, &["analyze", "cycles"]);
    assert!(out.status.success());
    assert!(stdout_str(&out).contains(CACHE_NOTICE));
    assert!(stdout_str(&out).contains("mutual recursion"));

    // impact (also fully sorted) reproduces byte-identical results across
    // a miss/hit pair too.
    let hit = run_analyze_json(&root, &["impact", "helper"]);
    let rebuilt = run_analyze_json(&root, &["impact", "helper", "--no-cache"]);
    assert_eq!(hit, rebuilt, "cached and rebuilt impact reports agree");
}

#[test]
fn codegraph_gitignore_keeps_the_cache_out_of_user_repos() {
    let (_dir, root) = temp_project();
    init_fixture(&root);
    let _ = run_analyze_json(&root, &["cycles"]);
    assert_cache_files_exist(&cache_dir(&root));

    // `.codegraph/.gitignore` ignores everything inside `.codegraph/`
    // except itself — which covers `analysis/` without any cache-specific
    // entry. Pin the two load-bearing lines.
    let gitignore =
        fs::read_to_string(root.join(".codegraph").join(".gitignore")).expect(".gitignore exists");
    let lines: Vec<&str> = gitignore.lines().map(str::trim).collect();
    assert!(
        lines.contains(&"*"),
        "ignore-everything rule present: {gitignore}"
    );
    assert!(
        lines.contains(&"!.gitignore"),
        "self-exception present: {gitignore}"
    );
}

// =============================================================================
// invalidation on re-index
// =============================================================================

#[test]
fn reindex_that_changes_the_store_invalidates_the_cache() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let before = run_analyze_json(&root, &["cycles"]);
    assert_eq!(before["cycleCount"].as_u64(), Some(1), "ping/pong only");

    // Warm hit to prove the cache was valid before the re-index.
    let out = run_cli(&root, &["analyze", "cycles"]);
    assert!(stdout_str(&out).contains(CACHE_NOTICE));

    // Add a second mutual-recursion pair and re-index.
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
    let out = run_cli(&root, &["index", "--force", "--quiet"]);
    assert!(
        out.status.success(),
        "re-index failed: {}",
        stderr_str(&out)
    );

    // The stale snapshot must NOT be served: no notice, fresh ground truth.
    let out = run_cli(&root, &["analyze", "cycles"]);
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert!(
        !stdout_str(&out).contains(CACHE_NOTICE),
        "changed index fingerprint must force a rebuild: {}",
        stdout_str(&out)
    );

    let after = run_analyze_json(&root, &["cycles"]);
    assert_eq!(
        after["cycleCount"].as_u64(),
        Some(2),
        "rebuild and the refreshed cache both see ping/pong + tick/tock: {after}"
    );

    // The rebuild refreshed the snapshot — next run hits again.
    let out = run_cli(&root, &["analyze", "cycles"]);
    assert!(stdout_str(&out).contains(CACHE_NOTICE));
}

// =============================================================================
// --no-cache
// =============================================================================

#[test]
fn no_cache_flag_bypasses_a_valid_snapshot() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let warm = run_analyze_json(&root, &["cycles"]);
    let out = run_cli(&root, &["analyze", "cycles"]);
    assert!(
        stdout_str(&out).contains(CACHE_NOTICE),
        "precondition: cache is hot"
    );

    // --no-cache: full rebuild, never the notice, same answers.
    let out = run_cli(&root, &["analyze", "cycles", "--no-cache"]);
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    let human = stdout_str(&out);
    assert!(
        !human.contains(CACHE_NOTICE),
        "--no-cache must not serve (or claim) the snapshot: {human}"
    );
    assert!(human.contains("mutual recursion"));

    let rebuilt = run_analyze_json(&root, &["cycles", "--no-cache"]);
    assert_eq!(
        warm["cycleCount"], rebuilt["cycleCount"],
        "bypass changes the code path, not the answer"
    );

    // Every subcommand in the family accepts the flag.
    for args in [
        vec!["complexity", "--no-cache"],
        vec!["communities", "--no-cache"],
        vec!["dominators", "main", "--no-cache"],
        vec!["slice", "main", "--no-cache"],
        vec!["impact", "helper", "--no-cache"],
        vec!["taint", "main", "helper", "--no-cache"],
    ] {
        let mut full = vec!["analyze"];
        full.extend_from_slice(&args);
        full.push("--json");
        let out = run_cli(&root, &full);
        assert!(
            out.status.success(),
            "analyze {} --no-cache failed: {}",
            args.join(" "),
            stderr_str(&out)
        );
    }
}

// =============================================================================
// corruption fallback
// =============================================================================

#[test]
fn corrupt_cache_falls_back_to_rebuild_and_self_heals() {
    let (_dir, root) = temp_project();
    init_fixture(&root);
    let _ = run_analyze_json(&root, &["cycles"]);
    let dir = cache_dir(&root);
    assert_cache_files_exist(&dir);

    // Truncated/garbage graph snapshot: load fails silently → rebuild.
    fs::write(dir.join("graph.bin"), b"definitely not a postcard snapshot").unwrap();
    let out = run_cli(&root, &["analyze", "cycles"]);
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert!(
        !stdout_str(&out).contains(CACHE_NOTICE),
        "corrupt snapshot must not be served"
    );
    assert!(stdout_str(&out).contains("mutual recursion"));

    // The rebuild rewrote the snapshot — the cache is healthy again.
    let out = run_cli(&root, &["analyze", "cycles"]);
    assert!(stdout_str(&out).contains(CACHE_NOTICE));

    // Same story for a mangled meta envelope.
    fs::write(dir.join("meta.json"), b"{ not json").unwrap();
    let out = run_cli(&root, &["analyze", "cycles"]);
    assert!(out.status.success());
    assert!(!stdout_str(&out).contains(CACHE_NOTICE));
    let out = run_cli(&root, &["analyze", "cycles"]);
    assert!(stdout_str(&out).contains(CACHE_NOTICE));
}

// =============================================================================
// CODEGRAPH_ANALYSIS_CACHE_DIR override
// =============================================================================

#[test]
fn env_override_relocates_the_cache_outside_the_project() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let override_dir = tempfile::Builder::new()
        .prefix("codegraph-analysis-cache-override-")
        .tempdir()
        .expect("create override dir");
    let override_path = override_dir.path().canonicalize().unwrap();
    let envs = [(
        "CODEGRAPH_ANALYSIS_CACHE_DIR",
        override_path.to_str().unwrap(),
    )];

    // First run with the override: the snapshot lands under
    // <override>/<workspace-key>/, NOT under .codegraph/analysis/.
    let out = run_cli_env(&root, &["analyze", "cycles", "--json"], &envs);
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert!(
        !cache_dir(&root).exists(),
        "override must keep the cache out of .codegraph/"
    );
    let keyed: Vec<PathBuf> = fs::read_dir(&override_path)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(keyed.len(), 1, "exactly one per-project key dir: {keyed:?}");
    assert_cache_files_exist(&keyed[0]);

    // Second run with the override hits.
    let out = run_cli_env(&root, &["analyze", "cycles"], &envs);
    assert!(stdout_str(&out).contains(CACHE_NOTICE));

    // Without the override the default location is cold — no false hit
    // from the relocated snapshot.
    let out = run_cli(&root, &["analyze", "cycles"]);
    assert!(out.status.success());
    assert!(!stdout_str(&out).contains(CACHE_NOTICE));
    assert_cache_files_exist(&cache_dir(&root));
}

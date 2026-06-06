//! CLI integration tests for `codegraph context` — the token-budgeted,
//! dataflow-seeded context command (`--budget` / `--strategy`).
//!
//! Like `analyze_cli_test.rs`, the CLI is exercised end-to-end against the
//! built binary (`CARGO_BIN_EXE_codegraph`): a fixture project is
//! initialized and indexed through `codegraph init`, then the `context`
//! subcommand is run in both strategies. Real files, real SQLite, no mocks.
//!
//! Contracts pinned here:
//! - `--strategy classic` (and the default) is the pre-existing
//!   `ContextBuilder` output, unchanged and deterministic;
//! - `--strategy analysis` respects `--budget <tokens>` within ±10 %
//!   measured tokens (chars / 4, the engine's heuristic);
//! - when the bridged graph lacks type-flow metadata, the analysis
//!   strategy degrades to call-graph seeding and says so (JSON `notes`,
//!   `--verbose` stderr) instead of fabricating dataflow.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_codegraph")
}

/// Run the built binary with `cwd`, stdin closed (no interactive prompts),
/// `CODEGRAPH_NO_DAEMON=1` pinned like the rest of the CLI suite.
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

fn stderr_str(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// Run `context ... --json` and parse stdout.
fn run_context_json(cwd: &Path, args: &[&str]) -> serde_json::Value {
    let mut full: Vec<&str> = vec!["context"];
    full.extend_from_slice(args);
    full.push("--json");
    let out = run_cli(cwd, &full);
    assert!(
        out.status.success(),
        "context {} failed: {}",
        args.join(" "),
        stderr_str(&out)
    );
    serde_json::from_str(stdout_str(&out).trim()).unwrap_or_else(|e| {
        panic!(
            "context {} did not print valid JSON ({e}): {}",
            args.join(" "),
            stdout_str(&out)
        )
    })
}

/// Canonicalized tempdir (macOS /var → /private/var symlink parity).
fn temp_project() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix("codegraph-context-cli-test-")
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

/// The same TypeScript fixture shape as `analyze_cli_test.rs`: the call
/// chain `main → compute → helper` plus the `ping ↔ pong` pair. Pure
/// function→function call edges — deliberately NO classes/interfaces, so
/// the bridged graph carries no type-flow (`uses_type`) edges and the
/// analysis strategy must degrade to call-graph seeding.
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

/// The chars/4 token measure the CLI reports (and the trim targets).
fn measure_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

const TASK: &str = "how does main reach helper through compute";

// =============================================================================
// classic strategy — regression: output unchanged
// =============================================================================

#[test]
fn context_classic_default_output_is_unchanged_and_deterministic() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    // Default strategy is classic; explicit --strategy classic is identical.
    let default_run = run_cli(&root, &["context", TASK]);
    assert!(
        default_run.status.success(),
        "stderr: {}",
        stderr_str(&default_run)
    );
    let explicit_run = run_cli(&root, &["context", TASK, "--strategy", "classic"]);
    assert!(explicit_run.status.success());
    let repeat_run = run_cli(&root, &["context", TASK]);
    assert!(repeat_run.status.success());

    let default_out = stdout_str(&default_run);
    assert_eq!(
        default_out,
        stdout_str(&explicit_run),
        "--strategy classic must equal the default"
    );
    assert_eq!(
        default_out,
        stdout_str(&repeat_run),
        "classic output must be deterministic"
    );

    // The ContextBuilder markdown shape, untouched by the new machinery.
    assert!(default_out.contains("## Code Context"));
    assert!(default_out.contains("**Query:**"));
    assert!(
        !default_out.contains("trimmed to the requested token budget"),
        "no budget trim without --budget"
    );
    assert!(
        !default_out.contains("### Source Code"),
        "no analysis-engine source section in classic output"
    );
}

#[test]
fn context_classic_json_is_the_context_builder_json() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_context_json(&root, &[TASK]);
    // ContextBuilder's flattened TaskContext JSON shape — not the analysis
    // report.
    for key in ["entryPoints", "nodes", "edges", "codeBlocks", "stats"] {
        assert!(json.get(key).is_some(), "TaskContext key {key}: {json}");
    }
    assert!(json.get("strategy").is_none(), "not the analysis report");
    assert!(json.get("measuredTokens").is_none());
}

#[test]
fn context_classic_budget_trims_markdown_output() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let untrimmed = stdout_str(&run_cli(&root, &["context", TASK]));
    let out = run_cli(&root, &["context", TASK, "--budget", "80", "--verbose"]);
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    let trimmed = stdout_str(&out);
    assert!(
        untrimmed.len() > 80 * 4,
        "fixture output must exceed the budget for this test to bite"
    );
    // println! adds one trailing newline on top of the trimmed payload.
    assert!(trimmed.len() <= 80 * 4 + 1, "trimmed len {}", trimmed.len());
    assert!(trimmed.contains("trimmed to the requested token budget"));
    assert!(stderr_str(&out).contains("classic output trimmed"));
}

// =============================================================================
// analysis strategy — token budget
// =============================================================================

#[test]
fn context_analysis_respects_budget_within_ten_percent() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let budget = 120usize;
    let json = run_context_json(&root, &[TASK, "--strategy", "analysis", "--budget", "120"]);
    assert_eq!(json["strategy"].as_str(), Some("analysis"));
    assert_eq!(json["budgetTokens"].as_u64(), Some(budget as u64));
    assert_eq!(
        json["truncated"].as_bool(),
        Some(true),
        "fixture output must exceed 120 tokens: {json}"
    );

    let markdown = json["markdown"].as_str().expect("markdown string");
    let measured = json["measuredTokens"].as_u64().expect("measuredTokens") as usize;
    assert_eq!(
        measured,
        measure_tokens(markdown),
        "reported measure must match chars/4 of the markdown"
    );
    assert!(
        measured <= budget + budget / 10,
        "measured {measured} above +10% of budget {budget}"
    );
    assert!(
        measured + budget / 10 >= budget,
        "measured {measured} below -10% of budget {budget}"
    );
    assert!(markdown.contains("trimmed to the requested token budget"));
}

#[test]
fn context_analysis_without_budget_uses_adaptive_tier_untruncated() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_context_json(&root, &[TASK, "--strategy", "analysis"]);
    assert_eq!(json["strategy"].as_str(), Some("analysis"));
    assert!(json["budgetTokens"].is_null());
    assert_eq!(
        json["truncated"].as_bool(),
        Some(false),
        "tiny fixture fits the smallest tier budget: {json}"
    );

    let markdown = json["markdown"].as_str().expect("markdown string");
    assert!(markdown.contains("## Code Context"));
    assert!(markdown.contains("### Entry Points"));
    assert!(
        markdown.contains("### Source Code"),
        "clustered source slices expected: {markdown}"
    );
    assert!(markdown.contains("src/util.ts"));
    // Entry points resolve main/compute/helper from the task.
    assert!(json["entryPointCount"].as_u64().unwrap() >= 2, "{json}");
    assert!(json["fileCount"].as_u64().unwrap() >= 1);
}

#[test]
fn context_analysis_human_output_is_markdown() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let out = run_cli(&root, &["context", TASK, "--strategy", "analysis"]);
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    let stdout = stdout_str(&out);
    assert!(stdout.contains("## Code Context"));
    assert!(stdout.contains("### Source Code"));
}

// =============================================================================
// analysis strategy — honest degradation
// =============================================================================

#[test]
fn context_analysis_reports_call_graph_seeding_degradation() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    // The fixture has only function→function call edges — no classes, so
    // the bridge produces no uses_type edges and dataflow seeding cannot
    // run. The report must say so instead of pretending.
    let json = run_context_json(&root, &[TASK, "--strategy", "analysis"]);
    assert_eq!(json["seeding"].as_str(), Some("call-graph"));
    let notes: Vec<&str> = json["notes"]
        .as_array()
        .expect("notes array")
        .iter()
        .map(|n| n.as_str().unwrap_or_default())
        .collect();
    assert!(
        notes.iter().any(|n| n.contains("call-graph seeding")),
        "degradation note expected in notes: {notes:?}"
    );

    // --verbose surfaces the same note on stderr (stdout stays markdown).
    let out = run_cli(
        &root,
        &["context", TASK, "--strategy", "analysis", "--verbose"],
    );
    assert!(out.status.success());
    let stderr = stderr_str(&out);
    assert!(
        stderr.contains("call-graph seeding"),
        "verbose stderr should carry the degradation note: {stderr}"
    );
    assert!(stderr.contains("seeding=call-graph"));
    assert!(
        !stdout_str(&out).contains("call-graph seeding"),
        "notes go to stderr, not into the markdown payload"
    );
}

// =============================================================================
// shared CLI contract
// =============================================================================

#[test]
fn context_rejects_invalid_strategy() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let out = run_cli(&root, &["context", TASK, "--strategy", "quantum"]);
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("--strategy"));
}

#[test]
fn context_rejects_non_positive_budget() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    for bad in ["0", "abc"] {
        let out = run_cli(&root, &["context", TASK, "--budget", bad]);
        assert!(!out.status.success(), "--budget {bad} must be rejected");
        assert!(stderr_str(&out).contains("--budget"));
    }
    // A negative value is eaten by clap as an unexpected argument — still a
    // hard failure, just with clap's own message.
    let out = run_cli(&root, &["context", TASK, "--budget", "-5"]);
    assert!(!out.status.success(), "--budget -5 must be rejected");
    assert!(stderr_str(&out).contains("-5"));
}

#[test]
fn context_requires_initialized_project() {
    let (_dir, root) = temp_project();
    // No init.
    for strategy in ["classic", "analysis"] {
        let out = run_cli(&root, &["context", TASK, "--strategy", strategy]);
        assert!(!out.status.success(), "{strategy} must fail uninitialized");
        assert!(stderr_str(&out).contains("not initialized"));
    }
}

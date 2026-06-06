//! CLI integration tests for the `codegraph analyze` subcommand family —
//! the jfc-graph analysis engine (`codegraph-analysis`) running over the
//! bridged SQLite index.
//!
//! Like `cli_test.rs`, the CLI is exercised end-to-end against the built
//! binary (`CARGO_BIN_EXE_codegraph`): a fixture project is initialized and
//! indexed through `codegraph init`, then every `analyze` subcommand is run
//! with `--json` and its stable camelCase shape asserted. Real files, real
//! SQLite, no mocks.

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
        .prefix("codegraph-analyze-cli-test-")
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

/// A small TypeScript project with known analysis ground truth:
/// - the call chain `main → compute → helper`,
/// - the mutual-recursion pair `ping ↔ pong`,
/// - `compute` as the most complex function (loop + branch).
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

fn names_of(values: &[serde_json::Value]) -> Vec<&str> {
    values
        .iter()
        .map(|v| v["name"].as_str().unwrap_or_default())
        .collect()
}

// =============================================================================
// complexity
// =============================================================================

#[test]
fn analyze_complexity_json_reports_per_function_metrics() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(&root, &["complexity", "--top", "3"]);
    assert_eq!(json["functionsAnalyzed"].as_u64(), Some(5));
    assert_eq!(json["functionsTotal"].as_u64(), Some(5));

    let functions = json["functions"].as_array().expect("functions array");
    assert_eq!(functions.len(), 3, "--top caps the list");

    // compute (loop + branch) is the most complex function in the fixture.
    let first = &functions[0];
    assert_eq!(first["symbol"]["name"].as_str(), Some("compute"));
    assert_eq!(first["symbol"]["file"].as_str(), Some("src/util.ts"));
    assert_eq!(first["language"].as_str(), Some("typescript"));
    assert!(first["cyclomatic"].as_u64().unwrap() >= 3);
    assert!(first["cognitive"].as_u64().unwrap() >= 2);
    assert!(first["maxNesting"].as_u64().unwrap() >= 2);
    assert!(first["maintainabilityIndex"].as_f64().is_some());

    // Sorted cyclomatic-descending.
    let cyclo: Vec<u64> = functions
        .iter()
        .map(|f| f["cyclomatic"].as_u64().unwrap())
        .collect();
    let mut sorted = cyclo.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(cyclo, sorted);
}

// =============================================================================
// communities
// =============================================================================

#[test]
fn analyze_communities_json_separates_call_clusters() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(&root, &["communities"]);
    assert!(json["modularity"].is_number());
    assert!(
        json["multiMemberCount"].as_u64().unwrap() >= 2,
        "main/compute/helper and ping/pong form separate call clusters: {json}"
    );

    let communities = json["communities"].as_array().expect("communities array");
    // Sorted by size descending.
    let sizes: Vec<u64> = communities
        .iter()
        .map(|c| c["size"].as_u64().unwrap())
        .collect();
    let mut sorted = sizes.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(sizes, sorted);

    // The ping/pong pair lands in one community together.
    let has_recursion_pair = communities.iter().any(|c| {
        let names = names_of(c["members"].as_array().unwrap());
        names.contains(&"ping") && names.contains(&"pong")
    });
    assert!(has_recursion_pair, "ping/pong share a community: {json}");
}

// =============================================================================
// dominators
// =============================================================================

#[test]
fn analyze_dominators_json_chains_back_to_entry() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(&root, &["dominators", "main"]);
    assert_eq!(json["entry"]["name"].as_str(), Some("main"));
    assert_eq!(json["analyzed"].as_u64(), Some(2));

    let nodes = json["nodes"].as_array().expect("nodes array");
    let helper = nodes
        .iter()
        .find(|n| n["symbol"]["name"].as_str() == Some("helper"))
        .expect("helper reachable from main");
    assert_eq!(
        helper["immediateDominator"]["name"].as_str(),
        Some("compute"),
        "every path from main to helper passes through compute"
    );
    assert_eq!(helper["dominatorDepth"].as_u64(), Some(2));
}

// =============================================================================
// slice
// =============================================================================

#[test]
fn analyze_slice_json_walks_both_directions() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let fwd = run_analyze_json(&root, &["slice", "main"]);
    assert_eq!(fwd["direction"].as_str(), Some("forward"));
    assert_eq!(fwd["granularity"].as_str(), Some("call-graph"));
    assert_eq!(fwd["size"].as_u64(), Some(2));
    let fwd_names = names_of(fwd["nodes"].as_array().unwrap());
    assert!(fwd_names.contains(&"compute") && fwd_names.contains(&"helper"));
    assert!(
        fwd["note"].as_str().unwrap().contains("call-graph"),
        "honest granularity note present"
    );

    let bwd = run_analyze_json(&root, &["slice", "helper", "--direction", "bwd"]);
    assert_eq!(bwd["direction"].as_str(), Some("backward"));
    let bwd_names = names_of(bwd["nodes"].as_array().unwrap());
    assert!(bwd_names.contains(&"main") && bwd_names.contains(&"compute"));
}

#[test]
fn analyze_slice_rejects_invalid_direction() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let out = run_cli(
        &root,
        &["analyze", "slice", "main", "--direction", "sideways"],
    );
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("--direction"));
}

// =============================================================================
// cycles
// =============================================================================

#[test]
fn analyze_cycles_json_finds_mutual_recursion() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(&root, &["cycles"]);
    assert_eq!(json["cycleCount"].as_u64(), Some(1));

    let cycle = &json["cycles"][0];
    assert_eq!(cycle["kind"].as_str(), Some("mutualRecursion"));
    assert_eq!(cycle["size"].as_u64(), Some(2));
    let members = names_of(cycle["members"].as_array().unwrap());
    assert_eq!(members, vec!["ping", "pong"]);

    let suggestions = json["breakSuggestions"].as_array().unwrap();
    assert_eq!(suggestions.len(), 1, "one greedy break suggestion");
}

// =============================================================================
// impact (signature-edit cascade)
// =============================================================================

#[test]
fn analyze_impact_json_lists_direct_call_sites() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(
        &root,
        &[
            "impact",
            "helper",
            "--signature",
            "helper(x: number, y: number): number",
        ],
    );
    assert_eq!(json["target"]["name"].as_str(), Some("helper"));
    assert_eq!(
        json["newSignature"].as_str(),
        Some("helper(x: number, y: number): number")
    );
    assert_eq!(
        json["callSiteCount"].as_u64(),
        Some(1),
        "only compute calls helper"
    );
    assert_eq!(json["taskCount"].as_u64(), Some(1));

    let task = &json["tasks"][0];
    assert_eq!(task["file"].as_str(), Some("src/util.ts"));
    assert_eq!(task["callSites"][0]["caller"].as_str(), Some("compute"));
    assert!(task["instruction"].as_str().unwrap().contains("helper"));
}

// =============================================================================
// taint (source → sink paths)
// =============================================================================

#[test]
fn analyze_taint_json_connects_source_to_sink_with_edge_kinds() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(&root, &["taint", "main", "helper"]);
    assert_eq!(json["source"]["name"].as_str(), Some("main"));
    assert_eq!(json["sink"]["name"].as_str(), Some("helper"));
    assert_eq!(json["granularity"].as_str(), Some("call-graph"));
    assert_eq!(json["pathCount"].as_u64(), Some(1));

    let path = &json["paths"][0];
    let nodes = names_of(path["nodes"].as_array().unwrap());
    assert_eq!(nodes, vec!["main", "compute", "helper"]);
    let edge_kinds: Vec<&str> = path["edgeKinds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(edge_kinds, vec!["calls", "calls"]);
    assert!(
        json["note"].as_str().unwrap().contains("dataflow IR"),
        "honest capability note present"
    );
}

#[test]
fn analyze_taint_json_reports_no_paths_against_call_direction() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    // helper never reaches main following edge direction.
    let json = run_analyze_json(&root, &["taint", "helper", "main"]);
    assert_eq!(json["pathCount"].as_u64(), Some(0));
    assert_eq!(json["paths"].as_array().unwrap().len(), 0);
}

// =============================================================================
// shared CLI contract
// =============================================================================

#[test]
fn analyze_human_output_succeeds_without_json() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let out = run_cli(&root, &["analyze", "complexity"]);
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert!(stdout_str(&out).contains("Most complex functions"));

    let out = run_cli(&root, &["analyze", "cycles"]);
    assert!(out.status.success());
    assert!(stdout_str(&out).contains("mutual recursion"));
}

#[test]
fn analyze_unknown_symbol_exits_zero_with_message() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let out = run_cli(&root, &["analyze", "dominators", "noSuchSymbolAnywhere"]);
    assert!(out.status.success(), "missing symbol is not an error");
    assert!(stdout_str(&out).contains("not found"));
}

#[test]
fn analyze_requires_initialized_project() {
    let (_dir, root) = temp_project();
    // No init.
    let out = run_cli(&root, &["analyze", "cycles"]);
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("not initialized"));
}

//! CLI integration tests for the `codegraph analyze` subcommand family —
//! the analysis engine (`codegraph-analysis`) running over the
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

/// Run an analyze subcommand with `--json` and parse the full envelope —
/// `{"schemaVersion": N, "kind": "<kind>", "data": …}`.
fn run_analyze_envelope(cwd: &Path, args: &[&str]) -> serde_json::Value {
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

/// Run an analyze subcommand with `--json`, assert the envelope contract,
/// and return its `data` payload.
fn run_analyze_json(cwd: &Path, args: &[&str]) -> serde_json::Value {
    let envelope = run_analyze_envelope(cwd, args);
    assert!(
        envelope["schemaVersion"].as_u64().is_some(),
        "envelope carries schemaVersion: {envelope}"
    );
    assert!(
        envelope["kind"].as_str().is_some(),
        "envelope carries kind: {envelope}"
    );
    envelope["data"].clone()
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
// query (pipe-based DSL)
// =============================================================================

#[test]
fn analyze_query_valid_dsl_returns_rows() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(&root, &["query", r#"fn("main") | callees | depth 3"#]);
    assert_eq!(
        json["query"].as_str(),
        Some(r#"fn("main") | callees | depth 3"#)
    );
    let nodes = names_of(json["nodes"].as_array().expect("nodes array"));
    assert!(
        nodes.contains(&"compute") && nodes.contains(&"helper"),
        "main's transitive callees resolved: {json}"
    );
    assert_eq!(json["nodeCount"].as_u64(), Some(nodes.len() as u64));
    assert_eq!(json["truncated"].as_bool(), Some(false));
}

/// Every worked example in `analyze query --help` must actually run over a
/// bridged codegraph index (the engine's native adapters see more kinds
/// than the bridge carries — these are pinned to calls/contains data).
#[test]
fn analyze_query_help_examples_run_on_bridged_index() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    // Example 1: transitive callees.
    let json = run_analyze_json(&root, &["query", r#"fn("main") | callees | depth 3"#]);
    let nodes = names_of(json["nodes"].as_array().unwrap());
    assert!(nodes.contains(&"compute") && nodes.contains(&"helper"));

    // Example 2: shortest call path (hops in path order in `edges`).
    let json = run_analyze_json(&root, &["query", r#"path fn("main") -> fn("helper")"#]);
    let nodes = names_of(json["nodes"].as_array().unwrap());
    for expected in ["main", "compute", "helper"] {
        assert!(nodes.contains(&expected), "path node {expected}: {json}");
    }
    assert!(
        !json["edges"].as_array().unwrap().is_empty(),
        "path hops surface as edges: {json}"
    );

    // Example 3: strongly-connected components (the ping/pong pair).
    let json = run_analyze_json(&root, &["query", "scc"]);
    let nodes = names_of(json["nodes"].as_array().unwrap());
    assert!(
        nodes.contains(&"ping") && nodes.contains(&"pong"),
        "mutual recursion pair found: {json}"
    );
}

#[test]
fn analyze_query_human_output_renders_table() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let out = run_cli(&root, &["analyze", "query", r#"fn("main") | callees"#]);
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    let stdout = stdout_str(&out);
    assert!(stdout.contains("Query results"), "header: {stdout}");
    assert!(
        stdout.contains("KIND") && stdout.contains("NAME") && stdout.contains("LOCATION"),
        "table columns: {stdout}"
    );
    assert!(stdout.contains("compute"), "result row: {stdout}");
}

#[test]
fn analyze_query_why_includes_provenance() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(&root, &["query", r#"fn("main") | callees"#, "--why"]);
    let why = json["why"].as_array().expect("why array present");
    let compute = why
        .iter()
        .find(|w| w["symbol"]["name"].as_str() == Some("compute"))
        .expect("result row compute is explained");
    let has_main_predecessor = compute["steps"].as_array().unwrap().iter().any(|step| {
        step["predecessors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p.as_str().unwrap_or_default().contains("main"))
    });
    assert!(
        has_main_predecessor,
        "compute's provenance references seed main: {json}"
    );
}

#[test]
fn analyze_query_explain_prints_plan_without_executing() {
    // No init — --explain must not touch the index at all.
    let (_dir, root) = temp_project();

    let json = run_analyze_json(
        &root,
        &[
            "query",
            r#"fn("main") | callees | callees | callees"#,
            "--explain",
        ],
    );
    assert_eq!(json["kind"].as_str(), Some("pipe"));
    let steps: Vec<&str> = json["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect();
    assert!(
        steps.iter().any(|s| s.contains("Depth(3)")),
        "depth fusion applied by the optimiser: {steps:?}"
    );
    assert!(json["strategy"].is_string());

    // Human flavor.
    let out = run_cli(
        &root,
        &["analyze", "query", r#"fn("main") | callees"#, "--explain"],
    );
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert!(stdout_str(&out).contains("not executed"));
}

#[test]
fn analyze_query_syntax_error_exits_one_quoting_token() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let out = run_cli(&root, &["analyze", "query", r#"fn("main") | bogus_op"#]);
    assert!(!out.status.success(), "syntax errors exit non-zero");
    let stderr = stderr_str(&out);
    assert!(
        stderr.contains("bogus_op"),
        "offending token quoted: {stderr}"
    );

    // Same contract under --explain (parse error, no panic).
    let out = run_cli(
        &root,
        &["analyze", "query", r#"fn("main") | bogus_op"#, "--explain"],
    );
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("bogus_op"));
}

#[test]
fn analyze_query_aggregation_surfaces_in_metadata() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(&root, &["query", r#"count fn("ping")"#]);
    let metadata: Vec<&str> = json["metadata"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m.as_str().unwrap())
        .collect();
    assert!(
        metadata.iter().any(|m| m.starts_with("scalar = 1")),
        "count projected into metadata: {json}"
    );
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

// =============================================================================
// JSON envelope contract (every analyze --json payload)
// =============================================================================

#[test]
fn analyze_json_is_wrapped_in_versioned_envelope() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let envelope = run_analyze_envelope(&root, &["cycles"]);
    assert_eq!(envelope["schemaVersion"].as_u64(), Some(1));
    assert_eq!(envelope["kind"].as_str(), Some("cycles"));
    assert_eq!(envelope["data"]["cycleCount"].as_u64(), Some(1));

    // The kind discriminates per subcommand.
    let envelope = run_analyze_envelope(&root, &["slice", "main"]);
    assert_eq!(envelope["kind"].as_str(), Some("slice"));
    let envelope = run_analyze_envelope(&root, &["query", r#"fn("main") | callees"#]);
    assert_eq!(envelope["kind"].as_str(), Some("query"));
    let envelope = run_analyze_envelope(&root, &["query", r#"fn("main")"#, "--explain"]);
    assert_eq!(envelope["kind"].as_str(), Some("queryPlan"));
}

// =============================================================================
// fixture for the close-list subcommands (traits/types/generics/taint-suggest)
// =============================================================================

/// A fixture with known trait/type/generic/taint ground truth:
/// - `Shape` interface implemented by `Circle` and `Square`,
/// - `totalArea(shapes: Shape[])` (UsesType → trait expansion),
/// - `identity<T>` (signature-heuristic generic),
/// - `readUserInput` → `execQuery` via `pipeline` (taint naming).
fn write_close_fixture(root: &Path) {
    write(
        &root.join("src/shapes.ts"),
        r#"export interface Shape {
  area(): number;
}

export class Circle implements Shape {
  radius: number = 1;
  area(): number {
    return 3.14 * this.radius * this.radius;
  }
}

export class Square implements Shape {
  side: number = 2;
  area(): number {
    return this.side * this.side;
  }
}

export function totalArea(shapes: Shape[]): number {
  let total = 0;
  for (const shape of shapes) {
    total += shape.area();
  }
  return total;
}

export function identity<T>(value: T): T {
  return value;
}
"#,
    );
    write(
        &root.join("src/io.ts"),
        r#"export function readUserInput(): string {
  return "input";
}

export function execQuery(sql: string): void {
}

export function pipeline(): void {
  execQuery(readUserInput());
}
"#,
    );
}

fn init_close_fixture(root: &Path) {
    write_close_fixture(root);
    let out = run_cli(root, &["init"]);
    assert!(out.status.success(), "init failed: {}", stderr_str(&out));
}

/// Run `git` in the fixture with identity pinned (CI has no global config).
fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args([
            "-c",
            "user.email=test@codegraph.test",
            "-c",
            "user.name=codegraph-test",
        ])
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
}

// =============================================================================
// co-change
// =============================================================================

#[test]
fn analyze_co_change_mines_git_history_and_is_honest_without_it() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    // Phase 1: not a git repository → exit 0 with the honest note.
    let out = run_cli(&root, &["analyze", "co-change"]);
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert!(
        stdout_str(&out).contains("No git history"),
        "honest no-history note: {}",
        stdout_str(&out)
    );

    // Phase 2: util.ts and main.ts committed together twice → a real
    // cross-file pair at min support 2.
    git(&root, &["init", "-q"]);
    git(&root, &["add", "src"]);
    git(&root, &["commit", "-qm", "one"]);
    for (file, suffix) in [
        ("src/util.ts", "// touch a\n"),
        ("src/main.ts", "// touch b\n"),
    ] {
        let path = root.join(file);
        let mut content = fs::read_to_string(&path).unwrap();
        content.push_str(suffix);
        fs::write(&path, content).unwrap();
    }
    git(&root, &["add", "src"]);
    git(&root, &["commit", "-qm", "two"]);

    let json = run_analyze_json(&root, &["co-change", "--min-support", "2"]);
    assert_eq!(json["commitsAnalyzed"].as_u64(), Some(2));
    assert_eq!(json["minSupport"].as_u64(), Some(2));
    let pairs = json["pairs"].as_array().expect("pairs array");
    assert!(
        !pairs.is_empty(),
        "main.ts/util.ts symbols co-change twice: {json}"
    );
    let pair = &pairs[0];
    assert_eq!(pair["timesChangedTogether"].as_u64(), Some(2));
    assert!(pair["confidence"].as_f64().unwrap() > 0.0);
    // Pairs are cross-file by contract; same-file pairs are only counted.
    assert_ne!(
        pair["a"]["file"].as_str(),
        pair["b"]["file"].as_str(),
        "listed pairs are cross-file: {pair}"
    );
    assert!(json["sameFilePairCount"].as_u64().unwrap() > 0);

    // Seeded: every pair touches the seed symbol.
    let json = run_analyze_json(&root, &["co-change", "helper", "--min-support", "2"]);
    for pair in json["pairs"].as_array().unwrap() {
        assert!(
            pair["a"]["name"].as_str() == Some("helper")
                || pair["b"]["name"].as_str() == Some("helper"),
            "seeded pair touches helper: {pair}"
        );
    }
}

// =============================================================================
// coverage
// =============================================================================

/// LCOV covering helper/compute (and main) but not ping/pong.
fn write_lcov(root: &Path) -> String {
    let lcov = root.join("lcov.info");
    fs::write(
        &lcov,
        "SF:src/util.ts\nDA:1,5\nDA:2,5\nDA:3,2\nDA:5,1\nDA:8,3\nDA:9,4\nDA:10,4\nDA:16,1\nend_of_record\n\
         SF:src/main.ts\nDA:3,1\nDA:4,1\nend_of_record\n",
    )
    .unwrap();
    lcov.display().to_string()
}

#[test]
fn analyze_coverage_maps_lcov_onto_functions() {
    let (_dir, root) = temp_project();
    init_fixture(&root);
    let lcov = write_lcov(&root);

    let json = run_analyze_json(&root, &["coverage", "--lcov", &lcov]);
    assert_eq!(json["functionsTotal"].as_u64(), Some(5));
    assert!(json["functionsTested"].as_u64().unwrap() >= 2);
    assert!(json["functionsUntested"].as_u64().unwrap() >= 1);
    assert_eq!(json["lcovFiles"].as_u64(), Some(2));

    // --untested filters the listing to untested functions only.
    let json = run_analyze_json(&root, &["coverage", "--lcov", &lcov, "--untested"]);
    let functions = json["functions"].as_array().unwrap();
    assert!(!functions.is_empty());
    for function in functions {
        assert_eq!(function["tested"].as_bool(), Some(false));
        assert_eq!(function["coverageCount"].as_u64(), Some(0));
    }
    let symbols: Vec<serde_json::Value> = functions.iter().map(|f| f["symbol"].clone()).collect();
    let untested = names_of(&symbols);
    assert!(
        untested.contains(&"pong"),
        "pong has no covered lines: {untested:?}"
    );

    // Human output names the untested functions.
    let out = run_cli(
        &root,
        &["analyze", "coverage", "--lcov", &lcov, "--untested"],
    );
    assert!(out.status.success());
    assert!(stdout_str(&out).contains("untested"));
}

#[test]
fn analyze_coverage_unreadable_lcov_exits_one() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let out = run_cli(&root, &["analyze", "coverage", "--lcov", "missing.info"]);
    assert!(!out.status.success(), "missing LCOV file is an error");
    assert!(stderr_str(&out).contains("missing.info"));
}

#[test]
fn analyze_query_lcov_unblinds_untested_operator() {
    let (_dir, root) = temp_project();
    init_fixture(&root);
    let lcov = write_lcov(&root);

    // ping has no DA lines → untested keeps it.
    let json = run_analyze_json(
        &root,
        &["query", r#"fn("ping") | untested"#, "--lcov", &lcov],
    );
    let nodes = names_of(json["nodes"].as_array().unwrap());
    assert_eq!(nodes, vec!["ping"], "uncovered ping survives: {json}");

    // compute is covered → untested filters it out.
    let json = run_analyze_json(
        &root,
        &["query", r#"fn("compute") | untested"#, "--lcov", &lcov],
    );
    assert_eq!(
        json["nodes"].as_array().unwrap().len(),
        0,
        "covered compute is filtered: {json}"
    );
}

// =============================================================================
// validate
// =============================================================================

#[test]
fn analyze_validate_judges_arity_change_against_callers() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    // Arity change: compute (helper's only caller) needs review.
    let json = run_analyze_json(
        &root,
        &[
            "validate",
            "helper",
            "--params-before",
            "1",
            "--params-after",
            "2",
        ],
    );
    assert_eq!(json["target"]["name"].as_str(), Some("helper"));
    assert_eq!(json["isSafe"].as_bool(), Some(false));
    let incompatible = json["incompatible"].as_array().unwrap();
    assert_eq!(incompatible.len(), 1);
    assert_eq!(incompatible[0]["symbol"]["name"].as_str(), Some("compute"));
    assert!(
        incompatible[0]["reason"].as_str().unwrap().contains("2"),
        "reason names the new arity: {json}"
    );
    assert!(!json["callSites"].as_array().unwrap().is_empty());
    assert!(json["note"].as_str().unwrap().contains("call-graph"));

    // Unchanged arity: safe.
    let json = run_analyze_json(
        &root,
        &[
            "validate",
            "helper",
            "--params-before",
            "1",
            "--params-after",
            "1",
        ],
    );
    assert_eq!(json["isSafe"].as_bool(), Some(true));
    assert_eq!(json["incompatible"].as_array().unwrap().len(), 0);

    // Bad arity argument exits 1.
    let out = run_cli(
        &root,
        &[
            "analyze",
            "validate",
            "helper",
            "--params-before",
            "x",
            "--params-after",
            "2",
        ],
    );
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("--params-before"));
}

// =============================================================================
// traits
// =============================================================================

#[test]
fn analyze_traits_reports_hierarchies_and_clusters() {
    let (_dir, root) = temp_project();
    init_close_fixture(&root);

    let json = run_analyze_json(&root, &["traits"]);
    assert_eq!(json["traitCount"].as_u64(), Some(1));
    let hierarchy = &json["hierarchies"][0];
    assert_eq!(hierarchy["trait"]["name"].as_str(), Some("Shape"));
    assert_eq!(hierarchy["implementorCount"].as_u64(), Some(2));
    let implementors = names_of(hierarchy["implementors"].as_array().unwrap());
    assert_eq!(implementors, vec!["Circle", "Square"]);

    // totalArea manipulates Shape → clustered under it.
    let clusters = json["clusters"].as_array().unwrap();
    let shape_cluster = clusters
        .iter()
        .find(|c| c["primaryType"]["name"].as_str() == Some("Shape"))
        .expect("Shape cluster");
    let members = names_of(shape_cluster["functions"].as_array().unwrap());
    assert!(members.contains(&"totalArea"), "members: {members:?}");

    // Type filter narrows to the requested type.
    let json = run_analyze_json(&root, &["traits", "Shape"]);
    assert_eq!(json["traitCount"].as_u64(), Some(1));
    let json = run_analyze_json(&root, &["traits", "NoSuchType"]);
    assert_eq!(json["traitCount"].as_u64(), Some(0));

    // Human output renders the hierarchy.
    let out = run_cli(&root, &["analyze", "traits"]);
    assert!(out.status.success());
    let stdout = stdout_str(&out);
    assert!(stdout.contains("Shape") && stdout.contains("Circle"));
}

// =============================================================================
// centrality / critical
// =============================================================================

#[test]
fn analyze_centrality_ranks_symbols_descending() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(&root, &["centrality", "--top", "3"]);
    assert!(json["analyzed"].as_u64().unwrap() >= 5);
    let nodes = json["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 3, "--top caps the list: {json}");
    let scores: Vec<f64> = nodes.iter().map(|n| n["score"].as_f64().unwrap()).collect();
    let mut sorted = scores.clone();
    sorted.sort_by(|a, b| b.partial_cmp(a).unwrap());
    assert_eq!(scores, sorted, "sorted by score descending");

    let out = run_cli(&root, &["analyze", "centrality"]);
    assert!(out.status.success());
    assert!(stdout_str(&out).contains("Most central symbols"));
}

#[test]
fn analyze_critical_finds_articulation_nodes() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(&root, &["critical"]);
    let nodes = names_of(json["nodes"].as_array().unwrap());
    assert!(
        nodes.contains(&"compute"),
        "compute articulates main → helper: {json}"
    );
    assert!(json["bridgeCount"].as_u64().unwrap() >= 1);
    let bridge = &json["bridges"][0];
    assert!(bridge["from"]["name"].is_string() && bridge["to"]["name"].is_string());
    assert!(json["note"].as_str().unwrap().contains("undirected"));
}

// =============================================================================
// export
// =============================================================================

#[test]
fn analyze_export_emits_dot_for_graph_and_symbol_neighborhood() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    // Human output is the raw DOT document — pipeable, no decoration.
    let out = run_cli(&root, &["analyze", "export"]);
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    let dot = stdout_str(&out);
    assert!(dot.starts_with("digraph"), "raw DOT on stdout: {dot}");
    assert!(dot.contains("->"), "edges rendered: {dot}");
    assert!(dot.contains("compute"), "node labels rendered: {dot}");

    // JSON wraps the document plus scope metadata.
    let json = run_analyze_json(&root, &["export"]);
    assert_eq!(json["format"].as_str(), Some("dot"));
    assert_eq!(json["scope"].as_str(), Some("graph"));
    assert!(json["nodeCount"].as_u64().unwrap() >= 5);
    assert!(json["dot"].as_str().unwrap().starts_with("digraph"));

    // --symbol narrows to the neighborhood.
    let json = run_analyze_json(&root, &["export", "--symbol", "main", "--depth", "1"]);
    assert_eq!(json["scope"].as_str(), Some("subgraph"));
    assert_eq!(json["seed"]["name"].as_str(), Some("main"));
    assert!(json["nodeCount"].as_u64().unwrap() >= 2);

    // Unsupported formats are rejected up front.
    let out = run_cli(&root, &["analyze", "export", "--format", "svg"]);
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("--format"));
}

// =============================================================================
// types
// =============================================================================

#[test]
fn analyze_types_propagates_concrete_types_through_traits() {
    let (_dir, root) = temp_project();
    init_close_fixture(&root);

    // totalArea takes Shape[] → trait expansion delivers Circle and Square.
    let json = run_analyze_json(&root, &["types", "totalArea"]);
    assert_eq!(json["symbol"]["name"].as_str(), Some("totalArea"));
    let inputs: Vec<&str> = json["inputTypes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for expected in ["Shape", "Circle", "Square"] {
        assert!(inputs.contains(&expected), "{expected} flows in: {json}");
    }
    assert!(json["functionsAnnotated"].as_u64().unwrap() >= 1);

    // A function with no type references reports honestly empty.
    let json = run_analyze_json(&root, &["types", "readUserInput"]);
    assert_eq!(json["inputTypes"].as_array().unwrap().len(), 0);
    assert!(json["note"].as_str().unwrap().contains("No concrete types"));
}

// =============================================================================
// generics
// =============================================================================

#[test]
fn analyze_generics_lists_signature_heuristic_definitions_honestly() {
    let (_dir, root) = temp_project();
    init_close_fixture(&root);

    let json = run_analyze_json(&root, &["generics"]);
    // The bridge carries no generics metadata → engine instantiations are
    // empty, and the note says exactly why.
    assert_eq!(json["instantiationCount"].as_u64(), Some(0));
    assert!(json["note"].as_str().unwrap().contains("does not populate"));
    // The signature heuristic still finds identity<T>.
    let definitions = json["likelyGenericDefinitions"].as_array().unwrap();
    let identity = definitions
        .iter()
        .find(|d| d["symbol"]["name"].as_str() == Some("identity"))
        .expect("identity<T> detected");
    assert_eq!(identity["typeParams"][0].as_str(), Some("T"));

    // Filtered to a non-generic symbol → honest empty message, exit 0.
    let out = run_cli(&root, &["analyze", "generics", "totalArea"]);
    assert!(out.status.success());
    assert!(stdout_str(&out).contains("No generic definition matches"));
}

// =============================================================================
// taint --suggest
// =============================================================================

#[test]
fn analyze_taint_suggest_ranks_sources_and_sinks_by_name() {
    let (_dir, root) = temp_project();
    init_close_fixture(&root);

    let json = run_analyze_json(&root, &["taint", "--suggest"]);
    let sources = json["sources"].as_array().unwrap();
    let sinks = json["sinks"].as_array().unwrap();
    assert!(
        sources
            .iter()
            .any(|c| c["symbol"]["name"].as_str() == Some("readUserInput")),
        "readUserInput is source-named: {json}"
    );
    assert!(
        sinks
            .iter()
            .any(|c| c["symbol"]["name"].as_str() == Some("execQuery")),
        "execQuery is sink-named: {json}"
    );
    let pairs = json["pairs"].as_array().unwrap();
    assert!(!pairs.is_empty());
    assert!(pairs[0]["priority"].as_f64().unwrap() > 0.0);
    assert!(json["note"].as_str().unwrap().contains("naming"));

    // Bare `analyze taint` defaults to suggestion mode.
    let envelope = run_analyze_envelope(&root, &["taint"]);
    assert_eq!(envelope["kind"].as_str(), Some("taintSuggest"));

    // One symbol without --suggest is a usage error.
    let out = run_cli(&root, &["analyze", "taint", "readUserInput"]);
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("--suggest"));
}

// =============================================================================
// boundaries
// =============================================================================

#[test]
fn analyze_boundaries_is_honestly_empty_over_bridged_index() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(&root, &["boundaries"]);
    assert_eq!(json["boundaryCount"].as_u64(), Some(0));
    assert!(
        json["note"]
            .as_str()
            .unwrap()
            .contains("does not populate these keys"),
        "honest capability note: {json}"
    );
    assert_eq!(json["crossLanguageCalls"]["edgesEmitted"].as_u64(), Some(0));

    // Human output prints the capability note instead of silence.
    let out = run_cli(&root, &["analyze", "boundaries"]);
    assert!(out.status.success());
    assert!(
        stderr_str(&out).contains("No cross-language boundaries")
            || stdout_str(&out).contains("No cross-language boundaries"),
        "stdout: {} stderr: {}",
        stdout_str(&out),
        stderr_str(&out)
    );
}

// =============================================================================
// capabilities
// =============================================================================

#[test]
fn analyze_capabilities_lists_env_toggles_and_cascades() {
    // Pure environment read — no init required.
    let (_dir, root) = temp_project();

    let json = run_analyze_json(&root, &["capabilities"]);
    let capabilities = json["capabilities"].as_array().unwrap();
    assert_eq!(capabilities.len(), 6);
    let call_graph = capabilities
        .iter()
        .find(|c| c["name"].as_str() == Some("callGraph"))
        .expect("callGraph listed");
    assert_eq!(
        call_graph["envVar"].as_str(),
        Some("CODEGRAPH_ANALYSIS_CAP_CALL_GRAPH")
    );
    assert_eq!(call_graph["enabled"].as_bool(), Some(true));
    assert_eq!(
        call_graph["disables"][0].as_str(),
        Some("virtualValidation"),
        "dependency cascade surfaced: {call_graph}"
    );

    // A kill-switch env var disables the capability AND its dependents.
    let out = Command::new(bin())
        .args(["analyze", "capabilities", "--json"])
        .current_dir(&root)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_ANALYSIS_CAP_CALL_GRAPH", "0")
        .stdin(Stdio::null())
        .output()
        .expect("spawn codegraph binary");
    assert!(out.status.success());
    let envelope: serde_json::Value = serde_json::from_str(stdout_str(&out).trim()).unwrap();
    let capabilities = envelope["data"]["capabilities"].as_array().unwrap();
    let by_name = |name: &str| -> &serde_json::Value {
        capabilities
            .iter()
            .find(|c| c["name"].as_str() == Some(name))
            .unwrap()
    };
    assert_eq!(by_name("callGraph")["enabled"].as_bool(), Some(false));
    assert_eq!(by_name("callGraph")["envValue"].as_str(), Some("0"));
    assert_eq!(
        by_name("virtualValidation")["enabled"].as_bool(),
        Some(false),
        "cascade applied: {envelope}"
    );
    assert_eq!(by_name("typeUsage")["enabled"].as_bool(), Some(true));
}

// =============================================================================
// schema
// =============================================================================

#[test]
fn analyze_schema_prints_engine_json_schemas() {
    // Pure schema read — no init required.
    let (_dir, root) = temp_project();

    for kind in [
        "query_result",
        "entrypoint_summary",
        "context_result",
        "formatted_output",
    ] {
        let out = run_cli(&root, &["analyze", "schema", kind]);
        assert!(out.status.success(), "schema {kind} exits 0");
        let schema: serde_json::Value = serde_json::from_str(stdout_str(&out).trim())
            .unwrap_or_else(|e| panic!("schema {kind} is valid JSON ({e})"));
        assert!(schema["title"].is_string());
        assert_eq!(
            schema["properties"]["schema_version"]["type"].as_str(),
            Some("integer")
        );
    }

    let out = run_cli(&root, &["analyze", "schema", "bogus"]);
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("known kinds"));
}

// =============================================================================
// stats
// =============================================================================

#[test]
fn analyze_stats_counts_graph_and_estimates_reachability() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(&root, &["stats"]);
    assert_eq!(json["nodesByKind"]["function"].as_u64(), Some(5));
    assert!(json["nodeCount"].as_u64().unwrap() >= 5);
    assert!(json["edgesByKind"]["calls"].as_u64().unwrap() >= 4);
    assert_eq!(json["fileCount"].as_u64(), Some(2));
    assert!(
        json.get("reachability").is_none() || json["reachability"].is_null(),
        "reachability is opt-in: {json}"
    );

    let json = run_analyze_json(&root, &["stats", "--estimate-reachability", "--top", "5"]);
    let reachability = &json["reachability"];
    assert_eq!(
        reachability["method"].as_str(),
        Some("exact"),
        "small graphs get exact numbers: {json}"
    );
    let top = reachability["top"].as_array().unwrap();
    assert!(!top.is_empty() && top.len() <= 5);
    let main_entry = top
        .iter()
        .find(|e| e["symbol"]["name"].as_str() == Some("main"));
    if let Some(main_entry) = main_entry {
        assert!(
            main_entry["descendants"].as_f64().unwrap() >= 2.0,
            "main reaches compute and helper: {json}"
        );
    }

    let out = run_cli(&root, &["analyze", "stats", "--estimate-reachability"]);
    assert!(out.status.success());
    assert!(stdout_str(&out).contains("Bridged analysis graph"));
}

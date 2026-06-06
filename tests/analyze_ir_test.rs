//! CLI integration tests for the IR-backed `codegraph analyze` commands
//! (gap-matrix Tier 2, items 14–16): `analyze cfg`, `analyze dataflow`, and
//! `analyze slice|taint --value-level`.
//!
//! Like `analyze_cli_test.rs`, the CLI is exercised end-to-end against the
//! built binary (`CARGO_BIN_EXE_codegraph`): a fixture project is
//! initialized and indexed through `codegraph init`, then each command is
//! run with `--json` and its stable camelCase shape asserted. Real files,
//! real SQLite, real tree-sitter re-parses — no mocks. The pre-v5 test
//! NULLs the index's byte offsets in place to prove the honest fallback.

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

/// Run an analyze subcommand with `--json`, assert the
/// `{"schemaVersion": N, "kind": "<kind>", "data": …}` envelope contract,
/// and return its `data` payload.
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
    let envelope: serde_json::Value =
        serde_json::from_str(stdout_str(&out).trim()).unwrap_or_else(|e| {
            panic!(
                "analyze {} did not print valid JSON ({e}): {}",
                args.join(" "),
                stdout_str(&out)
            )
        });
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
        .prefix("codegraph-analyze-ir-test-")
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

/// A small polyglot project with known IR-analysis ground truth:
/// - `flow.rs` — the value-flow triple: `passes_data` forwards its parameter
///   into `target_fn`, `calls_no_data` calls it with a literal only (the
///   crafted fixture on which a value-level slice must differ from the
///   call-graph slice);
/// - `relay.ts` — `relay` with one param, one assignment, one return, and an
///   argument flow into `helper`; `compute` with a loop + branch (CFG block
///   variety); `helper` straight-line (exact CFG block count);
/// - `greet.rb` — Ruby, outside both CFG and dataflow rule coverage (the
///   honest capability note).
fn write_fixture(root: &Path) {
    write(
        &root.join("src/flow.rs"),
        r#"pub fn target_fn(x: i32) -> i32 {
    x + 1
}

pub fn passes_data(v: i32) -> i32 {
    let result = target_fn(v);
    result
}

pub fn calls_no_data() {
    target_fn(1);
}
"#,
    );
    write(
        &root.join("src/relay.ts"),
        r#"export function helper(v: number): number {
  return v + 1;
}

export function relay(v: number): number {
  const doubled = v * 2;
  helper(v);
  return doubled;
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
"#,
    );
    write(&root.join("src/greet.rb"), "def greet\n  puts 'hi'\nend\n");
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
// cfg
// =============================================================================

#[test]
fn analyze_cfg_json_has_expected_block_count_for_straight_line_function() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    // `helper` is straight-line: exactly ENTRY, EXIT, and one return block.
    let json = run_analyze_json(&root, &["cfg", "helper"]);
    assert_eq!(json["analyzed"].as_bool(), Some(true), "json: {json}");
    assert_eq!(json["language"].as_str(), Some("typescript"));
    assert_eq!(json["blockCount"].as_u64(), Some(3));
    assert_eq!(json["edgeCount"].as_u64(), Some(2));

    let blocks = json["blocks"].as_array().expect("blocks array");
    assert_eq!(blocks.len(), 3, "blockCount matches the list");
    assert_eq!(blocks[0]["kind"].as_str(), Some("entry"));
    assert_eq!(blocks[1]["kind"].as_str(), Some("exit"));
    assert_eq!(blocks[2]["kind"].as_str(), Some("normal"));

    let edges = json["edges"].as_array().expect("edges array");
    assert!(
        edges.iter().any(|e| e["kind"].as_str() == Some("return")),
        "return edge into EXIT: {json}"
    );
}

#[test]
fn analyze_cfg_json_builds_branch_and_loop_blocks() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    // `compute` has a for-loop wrapping an if/else.
    let json = run_analyze_json(&root, &["cfg", "compute"]);
    assert_eq!(json["analyzed"].as_bool(), Some(true), "json: {json}");
    assert!(json["skipReason"].is_null(), "no skip on success: {json}");
    assert!(
        json["blockCount"].as_u64().unwrap() >= 6,
        "loop + branch yield several blocks: {json}"
    );

    let blocks = json["blocks"].as_array().expect("blocks array");
    let kinds: Vec<&str> = blocks
        .iter()
        .map(|b| b["kind"].as_str().unwrap_or_default())
        .collect();
    assert!(kinds.contains(&"loop"), "loop header block: {kinds:?}");
    assert!(kinds.contains(&"branch"), "if/else branch block: {kinds:?}");

    let edges = json["edges"].as_array().expect("edges array");
    let edge_kinds: Vec<&str> = edges
        .iter()
        .map(|e| e["kind"].as_str().unwrap_or_default())
        .collect();
    assert!(
        edge_kinds.contains(&"loopBack"),
        "loop back-edge: {edge_kinds:?}"
    );
    assert!(
        edge_kinds.contains(&"branchFalse"),
        "branch outcome edges: {edge_kinds:?}"
    );

    // Every edge endpoint references a listed block id.
    let ids: Vec<u64> = blocks.iter().map(|b| b["id"].as_u64().unwrap()).collect();
    for e in edges {
        assert!(ids.contains(&e["from"].as_u64().unwrap()), "edge from: {e}");
        assert!(ids.contains(&e["to"].as_u64().unwrap()), "edge to: {e}");
    }
}

// Honesty: a language without CFG rules is an explicit capability note,
// never a silently empty graph.
#[test]
fn analyze_cfg_json_notes_unsupported_language_honestly() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(&root, &["cfg", "greet"]);
    assert_eq!(json["analyzed"].as_bool(), Some(false), "json: {json}");
    assert_eq!(json["skipReason"].as_str(), Some("unsupportedLanguage"));
    assert_eq!(json["blockCount"].as_u64(), Some(0));
    let note = json["note"].as_str().expect("note present");
    assert!(
        note.contains("Rust, TypeScript/TSX"),
        "note lists covered languages: {note}"
    );
    assert!(
        note.contains("capability gap"),
        "note is explicit about the gap: {note}"
    );

    // Human output surfaces the same note (no silent empty section).
    let out = run_cli(&root, &["analyze", "cfg", "greet"]);
    assert!(out.status.success());
    assert!(
        stdout_str(&out).contains("capability gap") || stderr_str(&out).contains("capability gap"),
        "human output carries the capability note"
    );
}

// =============================================================================
// dataflow
// =============================================================================

#[test]
fn analyze_dataflow_json_returns_defs_and_uses() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(&root, &["dataflow", "relay"]);
    assert_eq!(json["analyzed"].as_bool(), Some(true), "json: {json}");
    assert_eq!(json["language"].as_str(), Some("typescript"));

    // Defs: the parameter and the assignment.
    let params = json["params"].as_array().expect("params array");
    assert_eq!(params.len(), 1);
    assert_eq!(params[0]["name"].as_str(), Some("v"));
    assert_eq!(params[0]["position"].as_u64(), Some(0));

    let assignments = json["assignments"].as_array().expect("assignments array");
    assert!(
        assignments
            .iter()
            .any(|a| a["target"].as_str() == Some("doubled")),
        "assignment to doubled: {assignments:?}"
    );

    // Uses: the return and the argument flow into helper.
    let returns = json["returns"].as_array().expect("returns array");
    assert!(
        returns
            .iter()
            .any(|r| r["expression"].as_str() == Some("doubled")),
        "returns doubled: {returns:?}"
    );

    let flows = json["argFlows"].as_array().expect("argFlows array");
    assert!(
        flows.iter().any(|f| {
            f["callee"].as_str() == Some("helper") && f["sourceParam"].as_str() == Some("v")
        }),
        "param v flows into helper: {flows:?}"
    );
}

// Honesty: dataflow rules cover fewer languages than CFG rules.
#[test]
fn analyze_dataflow_json_notes_unsupported_language_honestly() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(&root, &["dataflow", "greet"]);
    assert_eq!(json["analyzed"].as_bool(), Some(false), "json: {json}");
    assert_eq!(json["skipReason"].as_str(), Some("unsupportedLanguage"));
    assert!(json["params"].as_array().unwrap().is_empty());
    let note = json["note"].as_str().expect("note present");
    assert!(
        note.contains("Rust, TypeScript/TSX"),
        "note lists covered languages: {note}"
    );
}

// =============================================================================
// slice --value-level
// =============================================================================

// The headline behavior: value-level slicing follows real value flow, so the
// caller that only passes a literal drops out of the slice while the
// call-graph slice keeps every caller.
#[test]
fn analyze_slice_value_level_differs_from_call_graph_on_crafted_fixture() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let call = run_analyze_json(&root, &["slice", "target_fn", "--direction", "bwd"]);
    assert_eq!(call["granularity"].as_str(), Some("call-graph"));
    assert!(
        call["irCoverage"].is_null(),
        "call-graph runs carry no IR coverage"
    );
    let call_names = names_of(call["nodes"].as_array().expect("nodes array"));
    assert!(call_names.contains(&"passes_data"), "names: {call_names:?}");
    assert!(
        call_names.contains(&"calls_no_data"),
        "call-graph slice includes every caller: {call_names:?}"
    );

    let value = run_analyze_json(
        &root,
        &["slice", "target_fn", "--direction", "bwd", "--value-level"],
    );
    assert_eq!(value["granularity"].as_str(), Some("value-level"));
    let value_names = names_of(value["nodes"].as_array().expect("nodes array"));
    assert!(
        value_names.contains(&"passes_data"),
        "value-flow caller kept: {value_names:?}"
    );
    assert!(
        !value_names.contains(&"calls_no_data"),
        "literal-only caller excluded at value level: {value_names:?}"
    );

    let coverage = &value["irCoverage"];
    assert!(
        coverage["functionsLowered"].as_u64().unwrap() >= 3,
        "the flow.rs triple lowers to IR: {coverage}"
    );
    assert_eq!(coverage["functionsMissingByteRange"].as_u64(), Some(0));
    assert!(
        value["note"].as_str().unwrap().contains("value-level"),
        "note states the granularity: {value}"
    );
}

// Honesty: an index without byte offsets (schema pre-v5) cannot anchor IR —
// the report falls back to call-graph granularity and says how to fix it.
#[test]
fn analyze_slice_value_level_pre_v5_index_falls_back_with_reindex_note() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    // Simulate a pre-v5 index: NULL out the stored byte offsets in place.
    let conn = rusqlite::Connection::open(root.join(".codegraph/codegraph.db"))
        .expect("open index database");
    conn.execute("UPDATE nodes SET start_byte = NULL, end_byte = NULL", [])
        .expect("null byte offsets");
    drop(conn);

    // --no-cache: the snapshot cache was built with byte ranges present.
    let json = run_analyze_json(
        &root,
        &[
            "slice",
            "target_fn",
            "--direction",
            "bwd",
            "--value-level",
            "--no-cache",
        ],
    );
    assert_eq!(
        json["granularity"].as_str(),
        Some("call-graph"),
        "degrades honestly: {json}"
    );
    let note = json["note"].as_str().expect("note present");
    assert!(
        note.contains("byte offsets"),
        "note names the missing data: {note}"
    );
    assert!(
        note.contains("Re-index") || note.contains("re-index"),
        "note says how to fix it: {note}"
    );
    let coverage = &json["irCoverage"];
    assert!(
        coverage["functionsMissingByteRange"].as_u64().unwrap() >= 3,
        "coverage counts the unanchorable functions: {coverage}"
    );
    assert_eq!(coverage["functionsLowered"].as_u64(), Some(0));

    // The call-graph fallback still answers the question.
    let names = names_of(json["nodes"].as_array().expect("nodes array"));
    assert!(
        names.contains(&"passes_data") && names.contains(&"calls_no_data"),
        "fallback slice keeps both callers: {names:?}"
    );
}

// =============================================================================
// taint --value-level
// =============================================================================

#[test]
fn analyze_taint_value_level_traces_flow_and_reports_absence() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    // passes_data forwards its parameter into target_fn: one value-level path.
    let flow = run_analyze_json(
        &root,
        &["taint", "passes_data", "target_fn", "--value-level"],
    );
    assert_eq!(flow["granularity"].as_str(), Some("value-level"));
    assert_eq!(flow["pathCount"].as_u64(), Some(1), "json: {flow}");
    let paths = flow["paths"].as_array().expect("paths array");
    let hop_names = names_of(paths[0]["nodes"].as_array().unwrap());
    assert_eq!(hop_names, vec!["passes_data", "target_fn"]);

    // calls_no_data passes only a literal: no value-level flow, said honestly
    // (a call-graph path exists, so silence would be a lie of omission).
    let dry = run_analyze_json(
        &root,
        &["taint", "calls_no_data", "target_fn", "--value-level"],
    );
    assert_eq!(dry["pathCount"].as_u64(), Some(0), "json: {dry}");
    assert!(
        dry["note"]
            .as_str()
            .unwrap()
            .contains("No value-level flow"),
        "honest absence note: {dry}"
    );
}

// --value-level is a tracing refinement; combining it with --suggest is a
// usage error, not a silent ignore.
#[test]
fn analyze_taint_value_level_rejects_suggest_mode() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let out = run_cli(&root, &["analyze", "taint", "--suggest", "--value-level"]);
    assert!(!out.status.success(), "usage error exits non-zero");
    assert!(
        stderr_str(&out).contains("--value-level applies to source"),
        "stderr explains the conflict: {}",
        stderr_str(&out)
    );
}

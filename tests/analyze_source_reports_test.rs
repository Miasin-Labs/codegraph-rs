//! Source-annotated `analyze` reports + source-level predicates
//! (gap-matrix rows `analysis_tools.rs` and `predicates.rs`):
//! `analyze slice|taint --source` (the engine's CPG report façade —
//! `analysis_tools::{program_slice, data_dependencies, taint_flow}`) and
//! the source-level `preconditions` enrichment of `analyze query`
//! (`predicates::extract_predicates`).
//!
//! Like `analyze_ir_test.rs`, the CLI is exercised end-to-end against the
//! built binary (`CARGO_BIN_EXE_codegraph`): a fixture project is
//! initialized and indexed through `codegraph init`, then each command is
//! run and its output asserted. Real files, real SQLite, real tree-sitter
//! re-parses — no mocks. The pre-v5 tests NULL the index's byte offsets in
//! place to prove the honest re-index notes. The engine path-cap test
//! drives the host library function directly (the CLI's rendered-flow cap
//! is a constant too large to exceed with a small fixture).

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

/// Run an analyze subcommand with `--json` and return the full
/// `{"schemaVersion": N, "kind": "<kind>", "data": …}` envelope.
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

/// Like [`run_analyze_envelope`] but asserts the envelope `kind` and
/// returns just the `data` payload.
fn run_analyze_json(cwd: &Path, kind: &str, args: &[&str]) -> serde_json::Value {
    let envelope = run_analyze_envelope(cwd, args);
    assert!(
        envelope["schemaVersion"].as_u64().is_some(),
        "envelope carries schemaVersion: {envelope}"
    );
    assert_eq!(
        envelope["kind"].as_str(),
        Some(kind),
        "envelope kind: {envelope}"
    );
    envelope["data"].clone()
}

/// Canonicalized tempdir (macOS /var → /private/var symlink parity).
fn temp_project() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix("codegraph-analyze-source-test-")
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

/// Rust fixture with known ground truth:
/// - the value-flow triple (`passes_data` forwards its parameter into
///   `target_fn`; `calls_no_data` passes only a literal) — the crafted
///   fixture on which the engine's source-annotated slice (value-level
///   when byte offsets are present) must exclude the literal-only caller;
/// - `gated` calls `danger` behind two nested `if` guards — the
///   preconditions ground truth (outermost-first condition ordering).
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

pub fn danger() {}

pub fn gated(x: i32, y: i32) {
    if x > 5 {
        if y < 2 {
            danger();
        }
    }
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

/// Simulate a pre-v5 index: NULL out the stored byte offsets in place.
fn null_byte_offsets(root: &Path) {
    let conn = rusqlite::Connection::open(root.join(".codegraph/codegraph.db"))
        .expect("open index database");
    conn.execute("UPDATE nodes SET start_byte = NULL, end_byte = NULL", [])
        .expect("null byte offsets");
}

// =============================================================================
// analyze slice --source
// =============================================================================

// Normal: the annotated slice report renders `name (file:line)` source
// anchors, rides value-level fidelity (the literal-only caller is excluded),
// and carries the one-hop data-dependency companion report.
#[test]
fn analyze_slice_source_renders_annotated_lines_with_value_fidelity() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(
        &root,
        "sliceSource",
        &["slice", "target_fn", "--direction", "bwd", "--source"],
    );
    assert_eq!(json["symbol"].as_str(), Some("target_fn"));
    assert_eq!(json["direction"].as_str(), Some("backward"));

    let report = json["report"].as_str().expect("annotated report present");
    assert!(
        report.contains("backward slice of `target_fn`"),
        "engine heading: {report}"
    );
    assert!(
        report.contains("passes_data (src/flow.rs:"),
        "source-annotated entry `name (file:line)`: {report}"
    );
    assert!(
        !report.contains("calls_no_data"),
        "value-level fidelity excludes the literal-only caller: {report}"
    );

    let deps = json["dataDependencies"]
        .as_str()
        .expect("data-dependency report present");
    assert!(
        deps.contains("data dependencies of `target_fn`") && deps.contains("passes_data"),
        "one-hop dependency companion: {deps}"
    );

    let coverage = &json["coverage"];
    assert_eq!(coverage["functionsMissingByteRange"].as_u64(), Some(0));
    assert!(
        json["note"]
            .as_str()
            .unwrap()
            .contains("Value-level fidelity rides the index's byte offsets"),
        "note states the fidelity source: {json}"
    );

    // Human output prints the same annotated lines (no silent JSON-only data).
    let out = run_cli(
        &root,
        &[
            "analyze",
            "slice",
            "target_fn",
            "--direction",
            "bwd",
            "--source",
        ],
    );
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    let text = stdout_str(&out);
    assert!(
        text.contains("passes_data (src/flow.rs:"),
        "human output carries the annotation: {text}"
    );
}

// Honesty: an index without byte offsets (schema pre-v5) cannot back the
// value-level oracle — the note names the missing data and the fix, while
// the line-span annotation itself keeps working.
#[test]
fn analyze_slice_source_pre_v5_byte_ranges_note_says_reindex() {
    let (_dir, root) = temp_project();
    init_fixture(&root);
    null_byte_offsets(&root);

    // --no-cache: the snapshot cache was built with byte ranges present.
    let json = run_analyze_json(
        &root,
        "sliceSource",
        &[
            "slice",
            "target_fn",
            "--direction",
            "bwd",
            "--source",
            "--no-cache",
        ],
    );
    let note = json["note"].as_str().expect("note present");
    assert!(
        note.contains("byte offsets"),
        "note names the missing data: {note}"
    );
    assert!(
        note.contains("re-index") || note.contains("Re-index"),
        "note says how to fix it: {note}"
    );
    let coverage = &json["coverage"];
    assert!(
        coverage["functionsMissingByteRange"].as_u64().unwrap() >= 5,
        "coverage counts every degraded function: {coverage}"
    );
    // Line-span annotation still works on the degraded index.
    assert!(
        json["report"].as_str().unwrap().contains("(src/flow.rs:"),
        "annotations survive without byte offsets: {json}"
    );
}

// Contract: --source and --value-level are different oracles for the same
// question — combining them is a usage error, not a silent pick.
#[test]
fn analyze_slice_source_rejects_value_level_combination() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let out = run_cli(
        &root,
        &["analyze", "slice", "target_fn", "--source", "--value-level"],
    );
    assert!(!out.status.success(), "usage error exits non-zero");
    assert!(
        stderr_str(&out).contains("mutually exclusive"),
        "stderr explains the conflict: {}",
        stderr_str(&out)
    );
}

// =============================================================================
// analyze taint --source
// =============================================================================

// Normal: the annotated flow report renders the full path hop by hop with
// sanitizer status, through the CLI.
#[test]
fn analyze_taint_source_renders_annotated_flow() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(
        &root,
        "taintSource",
        &["taint", "passes_data", "target_fn", "--source"],
    );
    let report = json["report"].as_str().expect("annotated report present");
    assert!(
        report.contains("1 taint flow found"),
        "engine heading: {report}"
    );
    assert!(
        report.contains("passes_data (src/flow.rs:") && report.contains("target_fn (src/flow.rs:"),
        "both endpoints annotated: {report}"
    );
    assert!(
        report.contains("UNSANITIZED"),
        "sanitizer status rendered: {report}"
    );
    assert_eq!(json["maxPaths"].as_u64(), Some(25));

    // --source needs the tracing mode, not --suggest.
    let out = run_cli(&root, &["analyze", "taint", "--suggest", "--source"]);
    assert!(!out.status.success(), "usage error exits non-zero");
    assert!(
        stderr_str(&out).contains("--source applies to source"),
        "stderr explains the conflict: {}",
        stderr_str(&out)
    );
}

// The engine's path cap: flows beyond `max_paths` are summarized with the
// "raise max_paths" trailer, never silently dropped. Driven through the
// host library function (the CLI cap of 25 is too large to exceed with a
// small fixture); two same-named sources resolve to two flows.
#[test]
fn analyze_taint_source_caps_paths_as_the_engine_intends() {
    use codegraph::analyze::{SliceDirection, source_slice_report, source_taint_report};
    use codegraph_analysis::edges::{EdgeData as AEdgeData, EdgeKind as AEdgeKind};
    use codegraph_analysis::graph::CodeGraph as AnalysisGraph;
    use codegraph_analysis::nodes::{
        NodeData as ANodeData,
        NodeId as ANodeId,
        NodeKind as ANodeKind,
        Span as ASpan,
        Visibility as AVisibility,
    };

    let (_dir, root) = temp_project();

    /// A Function node anchored at `decl`'s byte position within `source`,
    /// with an **absolute** file path so the engine façade reads it
    /// regardless of the test process cwd.
    fn fn_node(path: &Path, name: &str, source: &str, decl: &str) -> ANodeData {
        let start = source.find(decl).expect("decl present in source");
        ANodeData {
            id: ANodeId::new(&path.display().to_string(), name, ANodeKind::Function),
            kind: ANodeKind::Function,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: path.to_path_buf(),
            span: ASpan {
                file: path.to_path_buf(),
                start_line: source[..start].matches('\n').count() as u32 + 1,
                start_col: 0,
                end_line: source[..start].matches('\n').count() as u32 + 1,
                end_col: 0,
                byte_range: start..start + decl.len(),
            },
            visibility: AVisibility::Public,
            metadata: std::collections::HashMap::new(),
            birth_revision: 0,
            last_modified_revision: 0,
            complexity: None,
            cfg: None,
            dataflow: None,
        }
    }

    let sink_src = "pub fn sink_exec(cmd: i32) -> i32 {\n    cmd\n}\n";
    let reader_src = "pub fn read_input(v: i32) -> i32 {\n    sink_exec(v)\n}\n";
    let sink_path = root.join("sink.rs");
    let reader_a_path = root.join("reader_a.rs");
    let reader_b_path = root.join("reader_b.rs");
    write(&sink_path, sink_src);
    write(&reader_a_path, reader_src);
    write(&reader_b_path, reader_src);

    let mut graph = AnalysisGraph::new();
    let sink = graph.add_node(fn_node(&sink_path, "sink_exec", sink_src, "fn sink_exec"));
    let reader_a = graph.add_node(fn_node(
        &reader_a_path,
        "read_input",
        reader_src,
        "fn read_input",
    ));
    let reader_b = graph.add_node(fn_node(
        &reader_b_path,
        "read_input",
        reader_src,
        "fn read_input",
    ));
    for (reader, file) in [(&reader_a, &reader_a_path), (&reader_b, &reader_b_path)] {
        graph
            .add_edge(
                reader,
                &sink,
                AEdgeData {
                    kind: AEdgeKind::Calls,
                    source_span: ASpan {
                        file: file.to_path_buf(),
                        start_line: 2,
                        start_col: 4,
                        end_line: 2,
                        end_col: 4,
                        byte_range: 0..0,
                    },
                    weight: 1.0,
                },
            )
            .unwrap();
    }

    // Two sources resolve under the one name → two flows; max_paths = 1
    // renders one and summarizes the rest.
    let capped = source_taint_report(&graph, &root, "read_input", "sink_exec", 1);
    assert!(
        capped.report.contains("2 taint flows found"),
        "both flows counted before the cap: {}",
        capped.report
    );
    assert!(
        capped.report.contains("1 more flow(s) (raise max_paths)"),
        "overflow summarized, not dropped: {}",
        capped.report
    );
    assert_eq!(capped.max_paths, 1);

    // Uncapped renders both fully — no trailer.
    let full = source_taint_report(&graph, &root, "read_input", "sink_exec", 25);
    assert!(
        !full.report.contains("raise max_paths"),
        "no trailer under the cap: {}",
        full.report
    );

    // Same entry cap contract on the slice façade ("... and N more").
    let slice = source_slice_report(&graph, &root, "sink_exec", SliceDirection::Backward, 1);
    assert!(
        slice.report.contains("more (raise max_nodes)"),
        "slice entries beyond the cap are summarized: {}",
        slice.report
    );
}

// =============================================================================
// analyze query '… | preconditions' — source-level guards
// =============================================================================

// Normal: a guarded call surfaces its actual guard expressions, outermost
// first (evaluation order), in both JSON and human output.
#[test]
fn analyze_query_preconditions_shows_guard_expressions() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(
        &root,
        "query",
        &["query", r#"fn("danger") | preconditions"#],
    );
    let names: Vec<&str> = json["nodes"]
        .as_array()
        .expect("nodes array")
        .iter()
        .map(|n| n["name"].as_str().unwrap_or_default())
        .collect();
    assert!(
        names.contains(&"danger") && names.contains(&"gated"),
        "preconditions walks callers: {names:?}"
    );

    let pre = &json["preconditions"];
    assert!(!pre.is_null(), "preconditions section attached: {json}");
    assert_eq!(pre["guardedCallCount"].as_u64(), Some(1), "section: {pre}");
    let guard = &pre["guards"][0];
    assert_eq!(guard["caller"]["name"].as_str(), Some("gated"));
    assert_eq!(guard["callee"].as_str(), Some("danger"));
    assert_eq!(guard["file"].as_str(), Some("src/flow.rs"));
    let conditions: Vec<&str> = guard["conditions"]
        .as_array()
        .expect("conditions array")
        .iter()
        .map(|c| c.as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        conditions,
        vec!["if x > 5", "if y < 2"],
        "guard expressions outermost first"
    );

    // Human output renders the same guards — never JSON-only.
    let out = run_cli(
        &root,
        &["analyze", "query", r#"fn("danger") | preconditions"#],
    );
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    let text = stdout_str(&out);
    assert!(
        text.contains("Guarding conditions (source-level):"),
        "human section heading: {text}"
    );
    assert!(
        text.contains("if x > 5") && text.contains("if y < 2"),
        "human output shows the guard expressions: {text}"
    );
}

// A query without the `preconditions` operator carries no section — the
// enrichment is operator-gated, not unconditional.
#[test]
fn analyze_query_without_preconditions_has_no_section() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(&root, "query", &["query", r#"fn("danger") | callers"#]);
    assert!(
        json["preconditions"].is_null(),
        "no section for non-preconditions queries: {json}"
    );
}

// Honesty: pre-v5 indexes (NULL byte offsets) cannot anchor source-level
// extraction — the section reports the gap and the fix instead of guessing
// a byte position.
#[test]
fn analyze_query_preconditions_pre_v5_byte_ranges_note_says_reindex() {
    let (_dir, root) = temp_project();
    init_fixture(&root);
    null_byte_offsets(&root);

    let json = run_analyze_json(
        &root,
        "query",
        &["query", r#"fn("danger") | preconditions"#, "--no-cache"],
    );
    let pre = &json["preconditions"];
    assert!(!pre.is_null(), "section still attached: {json}");
    assert_eq!(
        pre["guardedCallCount"].as_u64(),
        Some(0),
        "no fabricated guards: {pre}"
    );
    let note = pre["note"].as_str().expect("note present");
    assert!(
        note.contains("byte offsets"),
        "note names the missing data: {note}"
    );
    assert!(
        note.contains("re-index") || note.contains("Re-index"),
        "note says how to fix it: {note}"
    );
}

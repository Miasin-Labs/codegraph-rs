//! Integration tests for flag-gated field carrying through the analysis
//! bridge (gap-matrix row `partial.rs`, close-list #18 host side) and the
//! partial-struct views it lights up in `context --strategy analysis`.
//!
//! Real pipeline, no mocks: a fixture project is indexed with
//! `CodeGraph::init` + `index_all`, the 10-field struct is hand-planted as
//! `property` rows (the TS extractor emits class fields as `method` nodes,
//! so planting is the established way to exercise the field paths — see
//! `analysis_bridge_test.rs` — and also proves the bridge works on any DB
//! shape), then bridged with and without `BridgeOptions::include_fields`.
//!
//! Covers the three contract points:
//! 1. the bridged-with-flag graph carries engine-typed field data
//!    (`partial::STRUCT_FIELDS_KEY` / `ACCESSED_FIELDS_KEY`) with no node
//!    explosion;
//! 2. `context --strategy analysis` renders the partial view (only the
//!    flow-touched fields) over that graph, and notes honestly without it;
//! 3. the snapshot cache never serves a graph bridged under one flag state
//!    to a request under the other.

use std::path::Path;

use codegraph::analysis_bridge::{
    BridgeOptions,
    BridgeResult,
    build_analysis_graph,
    build_analysis_graph_cached_with_options,
    build_analysis_graph_with_options,
};
use codegraph::context_analysis::{AnalysisContextOptions, build_analysis_context};
use codegraph::db::{DatabaseConnection, QueryBuilder, get_database_path};
use codegraph::{CodeGraph, Edge, EdgeKind, IndexOptions, Language, Node, NodeKind, Visibility};
use codegraph_analysis::fingerprint::Fingerprintable;
use codegraph_analysis::nodes::NodeKind as ANodeKind;
use codegraph_analysis::partial;
use tempfile::TempDir;

// =============================================================================
// Fixture: BigConfig (10 fields) + loadConfig (touches 2 of them)
// =============================================================================

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

fn write_fixture(root: &Path) {
    write(
        &root.join("src/config.ts"),
        r#"export class BigConfig {
  describe() { return "config"; }
}
"#,
    );
    write(
        &root.join("src/load.ts"),
        r#"import { BigConfig } from './config';

export function loadConfig() {
  const cfg = new BigConfig();
  return cfg.describe();
}
"#,
    );
}

fn index_fixture(root: &Path) {
    let cg = if CodeGraph::is_initialized(root) {
        CodeGraph::open_sync(root).expect("open")
    } else {
        CodeGraph::init_sync(root).expect("init")
    };
    cg.index_all(&IndexOptions::default()).expect("index_all");
    cg.close();
}

fn open_queries(root: &Path) -> (DatabaseConnection, QueryBuilder) {
    let conn = DatabaseConnection::open(get_database_path(root)).expect("open db");
    let qb = QueryBuilder::new(conn.get_db().expect("get_db"));
    (conn, qb)
}

fn db_node_id(qb: &QueryBuilder, name: &str, kind: NodeKind) -> String {
    qb.get_nodes_by_name(name)
        .unwrap()
        .into_iter()
        .find(|n| n.kind == kind)
        .unwrap_or_else(|| panic!("no {kind:?} node named {name:?} in DB"))
        .id
}

/// The ten field names, `host`/`port` first — `loadConfig` touches exactly
/// those two.
fn field_names() -> Vec<String> {
    let mut names = vec!["host".to_string(), "port".to_string()];
    for i in 3..=10 {
        names.push(format!("extra{i:02}"));
    }
    names
}

/// Plant 10 `property` rows under `BigConfig` (contains edges) and
/// `references` edges from `loadConfig` to `host` + `port`. Signature
/// styles are mixed on purpose: `host` is annotation-style (`name: Type`),
/// `port` is declaration-style (`Type name`) — both shapes real extractors
/// write.
fn plant_fields(qb: &QueryBuilder) {
    let class_id = db_node_id(qb, "BigConfig", NodeKind::Class);
    let fn_id = db_node_id(qb, "loadConfig", NodeKind::Function);

    for (i, name) in field_names().iter().enumerate() {
        let mut prop = Node::new(
            format!("property:planted{i:026}"),
            NodeKind::Property,
            name,
            format!("BigConfig::{name}"),
            "src/config.ts",
            Language::Typescript,
            2,
            2,
        );
        prop.signature = match name.as_str() {
            "host" => Some("host: string".to_string()),
            "port" => Some("number port".to_string()),
            _ => Some(format!("{name}: string")),
        };
        prop.visibility = Some(if name == "port" {
            Visibility::Private
        } else {
            Visibility::Public
        });
        qb.insert_node(&prop).expect("insert property node");
        qb.insert_edge(&Edge::new(&class_id, &prop.id, EdgeKind::Contains))
            .expect("class contains property");
        if name == "host" || name == "port" {
            qb.insert_edge(&Edge::new(&fn_id, &prop.id, EdgeKind::References))
                .expect("function references property");
        }
    }
}

/// Indexed fixture + planted fields; callers open their own connection.
fn fixture(root: &Path) {
    write_fixture(root);
    index_fixture(root);
    let (_conn, qb) = open_queries(root);
    plant_fields(&qb);
}

fn struct_fields_meta(result: &BridgeResult) -> Option<String> {
    let node = result
        .graph
        .find_by_name("BigConfig")
        .into_iter()
        .find(|n| n.kind == ANodeKind::Struct)
        .expect("BigConfig bridged as Struct");
    node.metadata.get(partial::STRUCT_FIELDS_KEY).cloned()
}

fn with_fields() -> BridgeOptions {
    BridgeOptions {
        include_fields: true,
    }
}

// =============================================================================
// 1. Bridged-with-flag graph contains engine-typed field data
// =============================================================================

#[test]
fn flag_gated_bridge_carries_engine_field_metadata() {
    let dir = TempDir::new().unwrap();
    fixture(dir.path());
    let (_conn, qb) = open_queries(dir.path());

    // Default bridge: legacy JSON name-array fold, no engine encoding.
    let without = build_analysis_graph(&qb).expect("bridge without flag");
    let legacy = struct_fields_meta(&without).expect("legacy fields fold present");
    let legacy_names: Vec<String> =
        serde_json::from_str(&legacy).expect("default fold is a JSON array");
    assert_eq!(legacy_names.len(), 10);
    assert!(
        partial::parse_fields_metadata(&legacy).is_empty(),
        "legacy fold must not masquerade as engine field data"
    );
    assert_eq!(without.stats.struct_fields_registered, 0);
    assert_eq!(without.stats.accessed_fields_registered, 0);

    // Flag-gated bridge: engine-typed registration via partial::set_*.
    let with = build_analysis_graph_with_options(&qb, &with_fields()).expect("bridge with flag");

    // No node explosion: fields ride metadata, never nodes.
    assert_eq!(
        with.graph.node_count(),
        without.graph.node_count(),
        "include_fields must not change the analysis node count"
    );
    assert_eq!(with.graph.edge_count(), without.graph.edge_count());

    // The struct decodes to 10 typed FieldInfos through the engine API.
    let encoded = struct_fields_meta(&with).expect("engine fields registered");
    let infos = partial::parse_fields_metadata(&encoded);
    assert_eq!(infos.len(), 10, "all 10 planted fields carried");
    let host = infos.iter().find(|f| f.name == "host").expect("host");
    assert_eq!(host.type_str, "string", "annotation-style signature parsed");
    assert!(host.is_public);
    let port = infos.iter().find(|f| f.name == "port").expect("port");
    assert_eq!(
        port.type_str, "number",
        "declaration-style signature parsed"
    );
    assert!(!port.is_public);
    assert_eq!(with.stats.struct_fields_registered, 10);
    assert_eq!(with.stats.accessed_fields_registered, 1);
    assert_eq!(with.stats.fields_skipped_invalid, 0);

    // End-to-end: the engine's partial view lights up over bridged data.
    let struct_id = with
        .graph
        .find_by_name("BigConfig")
        .into_iter()
        .find(|n| n.kind == ANodeKind::Struct)
        .unwrap()
        .id
        .clone();
    let fn_id = with
        .graph
        .find_by_name("loadConfig")
        .into_iter()
        .find(|n| n.kind == ANodeKind::Function)
        .unwrap()
        .id
        .clone();
    let view = partial::try_get_partial_struct(&with.graph, &struct_id, &fn_id)
        .expect("partial view over bridged data");
    assert!(view.is_partial);
    assert_eq!(view.all_fields.len(), 10);
    let visible: Vec<&str> = view
        .visible_fields()
        .into_iter()
        .map(|f| f.name.as_str())
        .collect();
    assert_eq!(visible, vec!["host", "port"]);

    // Determinism holds with the flag on: the registration pass iterates in
    // a fixed order, so re-bridging yields the identical fingerprint.
    let again = build_analysis_graph_with_options(&qb, &with_fields()).expect("re-bridge");
    assert_eq!(
        with.graph.fingerprint(),
        again.graph.fingerprint(),
        "with-fields bridging must be deterministic"
    );
}

// =============================================================================
// 2. context --strategy analysis renders the partial view
// =============================================================================

#[test]
fn context_analysis_renders_partial_view_over_bridged_fields() {
    let dir = TempDir::new().unwrap();
    fixture(dir.path());
    let (_conn, qb) = open_queries(dir.path());

    // With fields: the partial view section appears, showing only the two
    // flow-touched fields plus an omitted count.
    let with = build_analysis_graph_with_options(&qb, &with_fields()).expect("bridge");
    let report = build_analysis_context(
        &with.graph,
        dir.path(),
        "how does loadConfig use BigConfig",
        &AnalysisContextOptions::default(),
    );
    assert_eq!(report.partial_struct_views, 1, "notes: {:?}", report.notes);
    assert!(report.markdown.contains("### Partial struct views"));
    assert!(
        report
            .markdown
            .contains("`BigConfig` — src/config.ts (2 of 10 fields accessed)"),
        "got: {}",
        report.markdown
    );
    assert!(report.markdown.contains("- ✓ `host`: string (pub)"));
    assert!(report.markdown.contains("- ✓ `port`: number (priv)"));
    assert!(report.markdown.contains("accessed by `loadConfig`"));
    assert!(
        report
            .markdown
            .contains("- … 8 more fields not touched by the selected symbols")
    );
    assert!(
        !report.markdown.contains("- ✓ `extra03`"),
        "untouched fields must not be expanded"
    );

    // Without fields (default bridge): no fabricated section, and an honest
    // note naming the gate.
    let without = build_analysis_graph(&qb).expect("bridge without flag");
    let report = build_analysis_context(
        &without.graph,
        dir.path(),
        "how does loadConfig use BigConfig",
        &AnalysisContextOptions::default(),
    );
    assert_eq!(report.partial_struct_views, 0);
    assert!(!report.markdown.contains("Partial struct views"));
    assert!(
        report
            .notes
            .iter()
            .any(|n| n.contains("CODEGRAPH_ANALYSIS_FIELDS")),
        "expected the field-gate note, got: {:?}",
        report.notes
    );
}

// =============================================================================
// 3. Snapshot cache never leaks between flag states
// =============================================================================

#[test]
fn snapshot_cache_does_not_leak_between_flag_states() {
    let dir = TempDir::new().unwrap();
    fixture(dir.path());
    let (_conn, qb) = open_queries(dir.path());
    let off = BridgeOptions::default();
    let on = with_fields();

    let bridge_cached = |options: &BridgeOptions| {
        build_analysis_graph_cached_with_options(&qb, dir.path(), true, options)
            .expect("cached bridge")
    };

    // Cold store, then a same-flag hit.
    let first = bridge_cached(&off);
    assert!(!first.from_cache);
    let hit = bridge_cached(&off);
    assert!(hit.from_cache, "same flag state must hit the cache");
    assert!(
        partial::parse_fields_metadata(&struct_fields_meta(&hit.result).unwrap_or_default())
            .is_empty(),
        "fieldless snapshot stays fieldless"
    );

    // Flag flips are cache misses — never served the other state's graph.
    let flipped = bridge_cached(&on);
    assert!(
        !flipped.from_cache,
        "a fieldless snapshot must not serve a with-fields request"
    );
    assert_eq!(
        partial::parse_fields_metadata(&struct_fields_meta(&flipped.result).expect("fields")).len(),
        10
    );

    // The with-fields generation now hits, with stats + field data intact.
    let hit = bridge_cached(&on);
    assert!(hit.from_cache);
    assert_eq!(hit.result.stats.struct_fields_registered, 10);
    assert_eq!(
        partial::parse_fields_metadata(&struct_fields_meta(&hit.result).expect("fields")).len(),
        10,
        "cached with-fields snapshot round-trips the field metadata"
    );

    // Flip back: miss again, and the rebuilt graph is fieldless.
    let back = bridge_cached(&off);
    assert!(
        !back.from_cache,
        "a with-fields snapshot must not serve a fieldless request"
    );
    assert!(
        partial::parse_fields_metadata(&struct_fields_meta(&back.result).unwrap_or_default())
            .is_empty()
    );
    assert_eq!(back.result.stats.struct_fields_registered, 0);
}

// =============================================================================
// 4. CLI end-to-end: `context --strategy analysis --fields`
// =============================================================================

/// Run the built binary with `cwd`, stdin closed, daemon off, and the
/// fields env var scrubbed so only the CLI flag controls field carrying.
fn run_cli(cwd: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .args(args)
        .current_dir(cwd)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env_remove("CODEGRAPH_ANALYSIS_FIELDS")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("spawn codegraph binary")
}

#[test]
fn cli_context_fields_flag_renders_partial_views() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fixture(&root);

    let task = "how does loadConfig use BigConfig";

    // --fields: the bridged graph carries field metadata and the partial
    // view section renders.
    let out = run_cli(
        &root,
        &[
            "context",
            task,
            "--strategy",
            "analysis",
            "--fields",
            "--json",
        ],
    );
    assert!(
        out.status.success(),
        "context --fields failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim())
            .expect("context --json prints the report JSON");
    assert_eq!(
        report["partialStructViews"], 1,
        "notes: {}",
        report["notes"]
    );
    let markdown = report["markdown"].as_str().expect("markdown string");
    assert!(markdown.contains("### Partial struct views"));
    assert!(markdown.contains("`BigConfig` — src/config.ts (2 of 10 fields accessed)"));

    // Without the flag (env scrubbed): no fabricated section, honest note
    // pointing at both the flag and the env var.
    let out = run_cli(
        &root,
        &["context", task, "--strategy", "analysis", "--json"],
    );
    assert!(out.status.success());
    let report: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("report JSON");
    assert_eq!(report["partialStructViews"], 0);
    let notes = report["notes"].to_string();
    assert!(
        notes.contains("--fields") && notes.contains("CODEGRAPH_ANALYSIS_FIELDS"),
        "expected the field-gate note naming the flag and env var, got: {notes}"
    );

    // Classic strategy: --fields is ignored with a warning, not an error.
    let out = run_cli(&root, &["context", task, "--fields"]);
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--fields requires --strategy analysis"),
        "expected the classic-strategy warning, got: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

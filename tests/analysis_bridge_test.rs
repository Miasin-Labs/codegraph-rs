//! Integration tests for `src/analysis_bridge.rs` — bridging an indexed
//! codegraph SQLite database into the `codegraph-analysis` engine.
//!
//! Real pipeline, no mocks: a fixture project is indexed with
//! `CodeGraph::init` + `index_all` (extraction + resolution + SQLite), then
//! bridged via `build_analysis_graph(&QueryBuilder)` and exercised with real
//! analyses (communities, callers/callees, centrality, DSL queries).
//!
//! Fixture ground truth (verified against the real extractor):
//! - TS class fields like `speed: number = 0` are extracted as `method`
//!   nodes (→ Function), NOT `property` — the property/field fold-in path
//!   is therefore exercised with hand-planted rows.
//! - TS enums produce `enum` + `enum_member` nodes with `contains` edges →
//!   the `variants` metadata fold-in runs on the real pipeline.

use std::fs;
use std::path::Path;

use codegraph::analysis_bridge::{
    BridgeResult,
    UNRESOLVED_FILE,
    build_analysis_graph,
    map_node_kind,
};
use codegraph::db::{DatabaseConnection, QueryBuilder, get_database_path};
use codegraph::{CodeGraph, Edge, EdgeKind, IndexOptions, Language, Node, NodeKind};
use codegraph_analysis::edges::EdgeKind as AEdgeKind;
use codegraph_analysis::fingerprint::Fingerprintable;
use codegraph_analysis::nodes::{NodeId as ANodeId, NodeKind as ANodeKind};
use codegraph_analysis::{analysis, communities};
use tempfile::TempDir;

// =============================================================================
// Fixture
// =============================================================================

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

/// A small TypeScript project covering every mapping path: functions +
/// resolved calls (`helper`/`compute`), a class with methods (`Engine`), an
/// interface + `implements`, an enum with members, a cross-file import, an
/// instantiation, and an unresolvable call (`mysteryCall`).
fn write_fixture(root: &Path) {
    write(
        &root.join("src/util.ts"),
        r#"export enum Mode { Fast, Slow }
export function helper() { return 1; }
export function compute() { return helper() + 2; }
"#,
    );
    write(
        &root.join("src/main.ts"),
        r#"import { compute } from './util';

export interface Runnable {
  go(): number;
}

export class Engine implements Runnable {
  speed: number = 0;
  go() { return compute(); }
}

export function main() {
  const engine = new Engine();
  return engine.go();
}

export function lonely() {
  return mysteryCall();
}
"#,
    );
}

/// Index the fixture with the real pipeline, then drop the handle so tests
/// can open their own read connection.
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

fn bridge(root: &Path) -> BridgeResult {
    let (_conn, qb) = open_queries(root);
    build_analysis_graph(&qb).expect("bridge")
}

/// Find the single analysis node with the given name + kind.
fn node_id_by_name(result: &BridgeResult, name: &str, kind: ANodeKind) -> ANodeId {
    let matches: Vec<ANodeId> = result
        .graph
        .find_by_name(name)
        .into_iter()
        .filter(|n| n.kind == kind)
        .map(|n| n.id.clone())
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one {kind:?} named {name:?}, got {}",
        matches.len()
    );
    matches.into_iter().next().unwrap()
}

/// Look up the codegraph node id for `(name, kind)` straight from the DB.
fn db_node_id(qb: &QueryBuilder, name: &str, kind: NodeKind) -> String {
    qb.get_nodes_by_name(name)
        .unwrap()
        .into_iter()
        .find(|n| n.kind == kind)
        .unwrap_or_else(|| panic!("no {kind:?} node named {name:?} in DB"))
        .id
}

// =============================================================================
// Node mapping over the real pipeline
// =============================================================================

#[test]
fn bridges_nodes_with_expected_kinds_and_counts() {
    let dir = TempDir::new().unwrap();
    write_fixture(dir.path());
    index_fixture(dir.path());

    let (_conn, qb) = open_queries(dir.path());
    let result = build_analysis_graph(&qb).expect("bridge");

    // Expected mapped count derives from the DB through the public kind
    // mapping: every node whose kind maps, minus dedupe collisions.
    let all = qb.get_all_nodes().expect("nodes");
    let expect_mapped = all
        .iter()
        .filter(|n| map_node_kind(n.kind).is_some())
        .count();
    assert_eq!(result.stats.nodes_total, all.len());
    assert_eq!(
        result.stats.nodes_mapped + result.stats.nodes_deduped,
        expect_mapped
    );
    assert_eq!(
        result.stats.nodes_mapped + result.stats.nodes_skipped + result.stats.nodes_deduped,
        result.stats.nodes_total
    );
    // Graph contains exactly the mapped nodes + unresolved placeholders.
    assert_eq!(
        result.graph.node_count(),
        result.stats.nodes_mapped + result.stats.placeholder_nodes
    );
    // Every mapped codegraph id resolves through id_map.
    assert_eq!(
        result.id_map.len(),
        result.stats.nodes_mapped + result.stats.nodes_deduped
    );

    // Specific symbols landed with the right 5-kind mapping.
    node_id_by_name(&result, "helper", ANodeKind::Function);
    node_id_by_name(&result, "compute", ANodeKind::Function);
    node_id_by_name(&result, "main", ANodeKind::Function);
    node_id_by_name(&result, "go", ANodeKind::Function); // method → Function
    node_id_by_name(&result, "Engine", ANodeKind::Struct); // class → Struct
    node_id_by_name(&result, "Runnable", ANodeKind::Trait); // interface → Trait
    node_id_by_name(&result, "Mode", ANodeKind::Enum);
    // The two file nodes became Modules.
    assert_eq!(
        result.graph.nodes_by_kind(ANodeKind::Module).len(),
        2,
        "expected exactly the two file nodes as Modules"
    );

    // Skipped kinds never became nodes: the `./util` import node and the
    // `Fast`/`Slow` enum members.
    assert!(
        result.graph.find_by_name("./util").is_empty(),
        "import nodes must not be bridged"
    );
    assert!(
        result.graph.find_by_name("Fast").is_empty(),
        "enum_member nodes must not be bridged"
    );
    assert_eq!(result.stats.skipped_node_kinds.get("import"), Some(&1));
    assert_eq!(result.stats.skipped_node_kinds.get("enum_member"), Some(&2));
    let skipped_kind_total: usize = result.stats.skipped_node_kinds.values().sum();
    assert_eq!(skipped_kind_total, result.stats.nodes_skipped);

    // ...but the enum members were preserved as `variants` metadata on Mode.
    let mode = node_id_by_name(&result, "Mode", ANodeKind::Enum);
    let mode_node = result.graph.get_node(&mode).unwrap();
    let variants: Vec<String> =
        serde_json::from_str(mode_node.metadata.get("variants").expect("variants key")).unwrap();
    assert_eq!(variants, vec!["Fast".to_string(), "Slow".to_string()]);

    // Original identity is preserved in metadata.
    let engine = node_id_by_name(&result, "Engine", ANodeKind::Struct);
    let engine_node = result.graph.get_node(&engine).unwrap();
    assert_eq!(
        engine_node
            .metadata
            .get("codegraph_kind")
            .map(String::as_str),
        Some("class")
    );
    assert!(engine_node.metadata.contains_key("codegraph_id"));
    assert_eq!(
        engine_node.metadata.get("exported").map(String::as_str),
        Some("true")
    );
}

// =============================================================================
// Edge mapping over the real pipeline
// =============================================================================

#[test]
fn bridges_edges_calls_contains_implements_usestype() {
    let dir = TempDir::new().unwrap();
    write_fixture(dir.path());
    index_fixture(dir.path());
    let result = bridge(dir.path());

    let helper = node_id_by_name(&result, "helper", ANodeKind::Function);
    let compute = node_id_by_name(&result, "compute", ANodeKind::Function);
    let go = node_id_by_name(&result, "go", ANodeKind::Function);
    let main = node_id_by_name(&result, "main", ANodeKind::Function);
    let engine = node_id_by_name(&result, "Engine", ANodeKind::Struct);
    let runnable = node_id_by_name(&result, "Runnable", ANodeKind::Trait);

    // calls → Calls: same-file (compute→helper), cross-file resolved
    // (go→compute), and method dispatch (main→go).
    assert!(analysis::callers_of(&result.graph, &helper).contains(&compute));
    assert!(analysis::callees_of(&result.graph, &go).contains(&compute));
    assert!(analysis::callees_of(&result.graph, &main).contains(&go));

    // implements → Implements; contains → Contains (class → method).
    let engine_out = result.graph.get_edges_from(&engine);
    assert!(
        engine_out
            .iter()
            .any(|(to, e)| **to == runnable && e.kind == AEdgeKind::Implements),
        "Engine should Implements Runnable"
    );
    assert!(
        engine_out
            .iter()
            .any(|(to, e)| **to == go && e.kind == AEdgeKind::Contains),
        "Engine should Contain go()"
    );

    // instantiates (Function→Struct) → UsesType.
    let main_out = result.graph.get_edges_from(&main);
    assert!(
        main_out
            .iter()
            .any(|(to, e)| **to == engine && e.kind == AEdgeKind::UsesType),
        "main should UsesType Engine (instantiates)"
    );

    // Edge accounting holds together.
    assert_eq!(
        result.stats.edges_mapped + result.stats.edges_skipped + result.stats.edges_enriched,
        result.stats.edges_total
    );
    assert!(result.stats.edges_mapped > 0);
    // Mode→Fast/Slow contains edges were folded into metadata.
    assert_eq!(result.stats.edges_enriched, 2);
    let skipped_reason_total: usize = result.stats.skipped_edge_reasons.values().sum();
    assert_eq!(skipped_reason_total, result.stats.edges_skipped);
    // The only skips for this fixture are edges touching the import node.
    assert_eq!(
        result.stats.skipped_edge_reasons.get("target_not_mapped"),
        Some(&2),
        "contains+imports edges to the `./util` import node: {:?}",
        result.stats.skipped_edge_reasons
    );
}

#[test]
fn unresolved_calls_become_unresolved_call_edges_to_placeholders() {
    let dir = TempDir::new().unwrap();
    write_fixture(dir.path());
    index_fixture(dir.path());

    let (_conn, qb) = open_queries(dir.path());
    let unresolved = qb.get_unresolved_references().expect("unresolved");
    assert!(
        unresolved.iter().any(|r| r.reference_name == "mysteryCall"),
        "pipeline should leave mysteryCall unresolved; got {:?}",
        unresolved
            .iter()
            .map(|r| r.reference_name.as_str())
            .collect::<Vec<_>>()
    );

    let result = build_analysis_graph(&qb).expect("bridge");
    assert!(result.stats.unresolved_mapped >= 1);
    assert!(result.stats.placeholder_nodes >= 1);
    assert_eq!(
        result.stats.unresolved_mapped + result.stats.unresolved_skipped,
        result.stats.unresolved_total
    );

    let lonely = node_id_by_name(&result, "lonely", ANodeKind::Function);
    let placeholder = ANodeId::new(UNRESOLVED_FILE, "mysteryCall", ANodeKind::Function);
    let placeholder_node = result
        .graph
        .get_node(&placeholder)
        .expect("deterministic placeholder node should exist");
    assert_eq!(
        placeholder_node
            .metadata
            .get("placeholder")
            .map(String::as_str),
        Some("true")
    );

    let out = result.graph.get_edges_from(&lonely);
    assert!(
        out.iter().any(|(to, e)| **to == placeholder
            && matches!(&e.kind, AEdgeKind::UnresolvedCall(name) if name == "mysteryCall")),
        "lonely should have an UnresolvedCall(mysteryCall) edge"
    );
}

// =============================================================================
// Skipped-kind information folded into metadata (hand-planted rows)
// =============================================================================

/// The TS extractor doesn't emit `property` nodes for class fields, so the
/// `fields` / `accessed_fields` fold-in is exercised by planting rows
/// directly — which also proves the bridge works on any DB shape, not just
/// what today's extractors emit.
#[test]
fn property_rows_fold_into_fields_and_accessed_fields_metadata() {
    let dir = TempDir::new().unwrap();
    write_fixture(dir.path());
    index_fixture(dir.path());

    let (_conn, qb) = open_queries(dir.path());
    let engine_id = db_node_id(&qb, "Engine", NodeKind::Class);
    let main_id = db_node_id(&qb, "main", NodeKind::Function);

    let prop = Node::new(
        "property:fixture0000000000000000000000",
        NodeKind::Property,
        "color",
        "Engine::color",
        "src/main.ts",
        Language::Typescript,
        8,
        8,
    );
    qb.insert_node(&prop).expect("insert property node");
    qb.insert_edge(&Edge::new(&engine_id, &prop.id, EdgeKind::Contains))
        .expect("class contains property");
    qb.insert_edge(&Edge::new(&main_id, &prop.id, EdgeKind::References))
        .expect("function references property");

    let result = build_analysis_graph(&qb).expect("bridge");

    // The property never became a node...
    assert!(result.graph.find_by_name("color").is_empty());
    assert_eq!(result.stats.skipped_node_kinds.get("property"), Some(&1));

    // ...but both relationships survived as metadata.
    let engine = node_id_by_name(&result, "Engine", ANodeKind::Struct);
    let fields: Vec<String> = serde_json::from_str(
        result
            .graph
            .get_node(&engine)
            .unwrap()
            .metadata
            .get("fields")
            .expect("fields key on Engine"),
    )
    .unwrap();
    assert_eq!(fields, vec!["color".to_string()]);

    let main = node_id_by_name(&result, "main", ANodeKind::Function);
    let accessed: Vec<String> = serde_json::from_str(
        result
            .graph
            .get_node(&main)
            .unwrap()
            .metadata
            .get("accessed_fields")
            .expect("accessed_fields key on main"),
    )
    .unwrap();
    assert_eq!(accessed, vec!["color".to_string()]);

    // Both edges counted as enriched (plus the 2 enum-variant edges).
    assert_eq!(result.stats.edges_enriched, 4);
}

// =============================================================================
// End-to-end analyses over the bridged graph
// =============================================================================

#[test]
fn analyses_run_end_to_end_over_bridged_graph() {
    let dir = TempDir::new().unwrap();
    write_fixture(dir.path());
    index_fixture(dir.path());
    let result = bridge(dir.path());

    // Louvain community detection: total assignment coverage, sane labels.
    let communities = communities::louvain(&result.graph, 1.0, 42);
    assert_eq!(
        communities.assignments.len(),
        result.graph.node_count(),
        "every node gets a community"
    );
    assert!(communities.community_count >= 1);
    assert!(communities.assignments.len() >= communities.community_count as usize);

    // PageRank centrality over the call/containment structure.
    let ranked = analysis::centrality(&result.graph, 5, 0.85);
    assert!(!ranked.is_empty(), "centrality should rank nodes");

    // Connected components: at least one component exists.
    assert!(analysis::independent_module_count(&result.graph) >= 1);
}

#[test]
fn bridged_graph_powers_a_graph_session() {
    let dir = TempDir::new().unwrap();
    write_fixture(dir.path());
    index_fixture(dir.path());
    let result = bridge(dir.path());

    let node_count = result.graph.node_count();
    let (session, stats) = result.into_session(dir.path());
    assert_eq!(session.graph.node_count(), node_count);
    assert!(stats.nodes_mapped > 0);

    // DSL query over the bridged graph (no source re-parse happened).
    let out = session
        .query(r#"fn("compute") | callees"#, 4000)
        .expect("DSL query should run over bridged graph");
    assert!(
        out.text.contains("helper"),
        "callees of compute should include helper, got: {}",
        out.text
    );
}

// =============================================================================
// Determinism
// =============================================================================

#[test]
fn rebuild_yields_identical_fingerprint_and_ids() {
    let dir = TempDir::new().unwrap();
    write_fixture(dir.path());
    index_fixture(dir.path());

    // Bridge the same index twice through independent connections.
    let first = bridge(dir.path());
    let second = bridge(dir.path());
    assert_eq!(
        first.graph.fingerprint(),
        second.graph.fingerprint(),
        "same DB must bridge to the same fingerprint"
    );
    assert_eq!(first.id_map, second.id_map);
    assert_eq!(first.graph.node_count(), second.graph.node_count());
    assert_eq!(first.graph.edge_count(), second.graph.edge_count());

    // Re-index over the existing index (incremental path), bridge again.
    index_fixture(dir.path());
    let third = bridge(dir.path());
    assert_eq!(first.graph.fingerprint(), third.graph.fingerprint());

    // Nuke `.codegraph` and rebuild the SQLite index from absolute scratch:
    // analysis NodeIds are content-addressed (file + qualified name + kind),
    // so even a from-zero rebuild must produce the identical graph.
    let codegraph_dir = get_database_path(dir.path())
        .parent()
        .expect("db lives in .codegraph/")
        .to_path_buf();
    fs::remove_dir_all(&codegraph_dir).expect("remove .codegraph");
    index_fixture(dir.path());
    let fourth = bridge(dir.path());
    assert_eq!(
        first.graph.fingerprint(),
        fourth.graph.fingerprint(),
        "from-scratch rebuild must produce identical analysis graph"
    );
    assert_eq!(first.id_map, fourth.id_map);
}

#[test]
fn node_ids_are_content_addressed_not_positional() {
    let dir = TempDir::new().unwrap();
    write_fixture(dir.path());
    index_fixture(dir.path());
    let result = bridge(dir.path());

    // Recomputing an id from (file_path, qualified_name, kind) alone must
    // land on the bridged node.
    let helper = node_id_by_name(&result, "helper", ANodeKind::Function);
    let helper_node = result.graph.get_node(&helper).unwrap();
    let recomputed = ANodeId::new(
        &helper_node.file_path.to_string_lossy(),
        &helper_node.qualified_name,
        ANodeKind::Function,
    );
    assert_eq!(recomputed, helper);
}

// =============================================================================
// Invariant-violating rows are skipped, not panicking
// =============================================================================

#[test]
fn invariant_violating_rows_are_skipped_and_counted() {
    let dir = TempDir::new().unwrap();
    write_fixture(dir.path());
    index_fixture(dir.path());

    let (_conn, qb) = open_queries(dir.path());
    let baseline = build_analysis_graph(&qb).expect("bridge");

    // Hand-plant rows the analysis invariants reject:
    // 1. `calls` from a class (Struct) to a function — Calls requires
    //    Function→Function.
    let engine_id = db_node_id(&qb, "Engine", NodeKind::Class);
    let helper_id = db_node_id(&qb, "helper", NodeKind::Function);
    qb.insert_edge(&Edge::new(&engine_id, &helper_id, EdgeKind::Calls))
        .expect("insert bad calls edge");
    // 2. A `contains` edge whose source is a plain function — Contains
    //    requires a container source.
    qb.insert_edge(&Edge::new(&helper_id, &engine_id, EdgeKind::Contains))
        .expect("insert bad contains edge");
    // 3. A garbage edge kind, straight through SQL (bypasses the typed API).
    qb.db()
        .conn()
        .execute(
            "INSERT INTO edges (source, target, kind) VALUES (?1, ?2, 'banana')",
            [&engine_id, &helper_id],
        )
        .expect("insert unknown-kind edge");
    // 4. A dangling edge pointing at a node id that doesn't exist (only
    //    counts if this connection doesn't enforce the FK).
    let dangling_insert = qb.db().conn().execute(
        "INSERT INTO edges (source, target, kind) VALUES (?1, 'no_such_node', 'calls')",
        [&engine_id],
    );

    let result = build_analysis_graph(&qb).expect("bridge must not panic on bad rows");

    // All planted rows were read...
    let mut planted = 3;
    if dangling_insert.is_ok() {
        planted += 1;
    }
    assert_eq!(
        result.stats.edges_total,
        baseline.stats.edges_total + planted
    );
    // ...and every one of them was skipped with a recorded reason.
    assert_eq!(
        result.stats.edges_skipped,
        baseline.stats.edges_skipped + planted
    );
    assert_eq!(result.stats.edges_mapped, baseline.stats.edges_mapped);
    assert!(
        result
            .stats
            .skipped_edge_reasons
            .keys()
            .any(|k| k.starts_with("invariant_calls_")),
        "bad calls edge should be counted under an invariant reason: {:?}",
        result.stats.skipped_edge_reasons
    );
    assert!(
        result
            .stats
            .skipped_edge_reasons
            .keys()
            .any(|k| k.starts_with("invariant_contains_")),
        "bad contains edge should be counted under an invariant reason: {:?}",
        result.stats.skipped_edge_reasons
    );
    assert_eq!(
        result.stats.skipped_edge_reasons.get("unknown_edge_kind"),
        Some(&1)
    );
    if dangling_insert.is_ok() {
        assert_eq!(
            result.stats.skipped_edge_reasons.get("dangling_endpoint"),
            Some(&1)
        );
    }

    // The graph itself is unchanged by the rejected rows.
    assert_eq!(result.graph.node_count(), baseline.graph.node_count());
    assert_eq!(result.graph.edge_count(), baseline.graph.edge_count());
    assert_eq!(
        result.graph.fingerprint(),
        baseline.graph.fingerprint(),
        "skipped rows must leave the analysis graph untouched"
    );
}

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

#[test]
fn data_symbols_strings_and_ida_facts_bridge_into_constants_and_metadata() {
    let dir = TempDir::new().unwrap();
    write_fixture(dir.path());
    index_fixture(dir.path());

    let (_conn, qb) = open_queries(dir.path());
    let main_id = db_node_id(&qb, "main", NodeKind::Function);

    let data = Node::new(
        "data_symbol:global_counter",
        NodeKind::DataSymbol,
        "global_counter",
        "global_counter",
        "src/main.ts",
        Language::Typescript,
        1,
        1,
    );
    let data_written = Node::new(
        "data_symbol:global_state",
        NodeKind::DataSymbol,
        "global_state",
        "global_state",
        "src/main.ts",
        Language::Typescript,
        1,
        1,
    );
    let string = Node::new(
        "string_literal:test",
        NodeKind::StringLiteral,
        "hello ida",
        "hello ida",
        "src/main.ts",
        Language::Typescript,
        1,
        1,
    );
    let mem = Node::new(
        "data_symbol:mem:a1+148",
        NodeKind::DataSymbol,
        "mem:a1+148",
        "mem:a1+148",
        "src/main.ts",
        Language::Typescript,
        1,
        1,
    );
    let callarg = Node::new(
        "data_symbol:callarg:memcpy:10:2",
        NodeKind::DataSymbol,
        "callarg:memcpy:10:2",
        "callarg:memcpy:10:2",
        "src/main.ts",
        Language::Typescript,
        1,
        1,
    );
    let label = Node::new(
        "data_symbol:label:LABEL_1",
        NodeKind::DataSymbol,
        "label:LABEL_1",
        "label:LABEL_1",
        "src/main.ts",
        Language::Typescript,
        1,
        1,
    );
    qb.insert_nodes(&[data, data_written, string, mem, callarg, label])
        .expect("insert ida fact nodes");

    let mut memory_edge = Edge::new(&main_id, "data_symbol:mem:a1+148", EdgeKind::Reads);
    memory_edge.metadata = Some(serde_json::Map::from_iter([
        ("kind".to_string(), serde_json::json!("memory_access")),
        ("base".to_string(), serde_json::json!("a1")),
        ("offset".to_string(), serde_json::json!(148)),
    ]));
    let mut call_edge = Edge::new(
        &main_id,
        "data_symbol:callarg:memcpy:10:2",
        EdgeKind::References,
    );
    call_edge.metadata = Some(serde_json::Map::from_iter([
        ("kind".to_string(), serde_json::json!("call_argument_roles")),
        ("callee".to_string(), serde_json::json!("memcpy")),
        (
            "arguments".to_string(),
            serde_json::json!([{ "index": 0, "role": "write_dst", "expr": "dst" }]),
        ),
    ]));
    let mut cfg_edge = Edge::new(&main_id, "data_symbol:label:LABEL_1", EdgeKind::References);
    cfg_edge.metadata = Some(serde_json::Map::from_iter([
        ("kind".to_string(), serde_json::json!("ida_cfg")),
        ("role".to_string(), serde_json::json!("goto")),
        ("label".to_string(), serde_json::json!("LABEL_1")),
    ]));
    qb.insert_edges(&[
        Edge::new(&main_id, "data_symbol:global_counter", EdgeKind::Reads),
        Edge::new(&main_id, "data_symbol:global_state", EdgeKind::Writes),
        Edge::new(&main_id, "string_literal:test", EdgeKind::References),
        memory_edge,
        call_edge,
        cfg_edge,
    ])
    .expect("insert ida fact edges");

    let result = build_analysis_graph(&qb).expect("bridge");
    node_id_by_name(&result, "global_counter", ANodeKind::Constant);
    node_id_by_name(&result, "hello ida", ANodeKind::Constant);

    let main = node_id_by_name(&result, "main", ANodeKind::Function);
    let metadata = &result.graph.get_node(&main).unwrap().metadata;
    let reads: Vec<String> =
        serde_json::from_str(metadata.get("global_reads").expect("global reads")).unwrap();
    let writes: Vec<String> =
        serde_json::from_str(metadata.get("global_writes").expect("global writes")).unwrap();
    let strings: Vec<String> =
        serde_json::from_str(metadata.get("string_refs").expect("string refs")).unwrap();
    let memory: Vec<serde_json::Value> =
        serde_json::from_str(metadata.get("memory_accesses").expect("memory accesses")).unwrap();
    let call_roles: Vec<serde_json::Value> = serde_json::from_str(
        metadata
            .get("call_argument_roles")
            .expect("call argument roles"),
    )
    .unwrap();
    let cfg: Vec<serde_json::Value> =
        serde_json::from_str(metadata.get("ida_cfg").expect("ida cfg")).unwrap();
    assert_eq!(reads, vec!["global_counter".to_string()]);
    assert_eq!(writes, vec!["global_state".to_string()]);
    assert_eq!(strings, vec!["hello ida".to_string()]);
    assert_eq!(memory.len(), 1);
    assert_eq!(call_roles.len(), 1);
    assert_eq!(cfg.len(), 1);
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

// =============================================================================
// Byte ranges (host schema v5) — spans carry real tree-sitter byte offsets
// =============================================================================

#[test]
fn bridged_spans_carry_real_byte_ranges() {
    let dir = TempDir::new().unwrap();
    write_fixture(dir.path());
    index_fixture(dir.path());
    let result = bridge(dir.path());

    // A freshly indexed fixture stores tree-sitter byte offsets for every
    // extracted node, so no mapped node degrades to the 0..0 sentinel.
    assert_eq!(
        result.stats.nodes_missing_byte_range, 0,
        "fresh v5 index must not produce byte-less mapped nodes"
    );

    // The span's byte range slices the on-disk source back to the
    // declaration — the anchor `ir_map`/value-level analyses need.
    let helper = node_id_by_name(&result, "helper", ANodeKind::Function);
    let span = &result.graph.get_node(&helper).unwrap().span;
    assert_ne!(span.byte_range, 0..0, "helper span must carry real bytes");
    let source = fs::read_to_string(dir.path().join("src/util.ts")).unwrap();
    let snippet = &source[span.byte_range.clone()];
    assert!(
        snippet.starts_with("function helper()"),
        "byte range should slice to the declaration, got: {snippet:?}"
    );

    let engine = node_id_by_name(&result, "Engine", ANodeKind::Struct);
    let engine_span = &result.graph.get_node(&engine).unwrap().span;
    let main_src = fs::read_to_string(dir.path().join("src/main.ts")).unwrap();
    assert!(main_src[engine_span.byte_range.clone()].starts_with("class Engine"));
}

#[test]
fn nodes_without_byte_offsets_degrade_to_zero_range_and_are_counted() {
    let dir = TempDir::new().unwrap();
    write_fixture(dir.path());
    index_fixture(dir.path());

    // Plant a function row with NULL byte offsets — the shape of any row
    // indexed before schema v5 (the migration backfills NULL).
    let (_conn, qb) = open_queries(dir.path());
    let legacy = Node::new(
        "function:fixture0000000000000000000000",
        NodeKind::Function,
        "legacyFn",
        "src/legacy.ts::legacyFn",
        "src/legacy.ts",
        Language::Typescript,
        1,
        3,
    );
    assert_eq!(legacy.start_byte, None);
    qb.insert_node(&legacy).expect("insert legacy node");

    let result = build_analysis_graph(&qb).expect("bridge");
    assert_eq!(
        result.stats.nodes_missing_byte_range, 1,
        "exactly the planted byte-less node should be counted"
    );
    let legacy_aid = node_id_by_name(&result, "legacyFn", ANodeKind::Function);
    let span = &result.graph.get_node(&legacy_aid).unwrap().span;
    assert_eq!(
        span.byte_range,
        0..0,
        "NULL offsets degrade to the sentinel"
    );
}

#[test]
fn index_fingerprint_incorporates_schema_version() {
    let dir = TempDir::new().unwrap();
    write_fixture(dir.path());
    index_fixture(dir.path());

    let (_conn, qb) = open_queries(dir.path());
    let before = codegraph::analysis_bridge::compute_index_fingerprint(&qb).expect("fingerprint");

    // Simulate a future schema migration: only schema_versions changes —
    // counts, rowids, updated_at, and file hashes all stay identical.
    qb.db()
        .conn()
        .execute(
            "INSERT INTO schema_versions (version, applied_at, description) VALUES (99, 0, 'simulated')",
            [],
        )
        .unwrap();
    let after = codegraph::analysis_bridge::compute_index_fingerprint(&qb).expect("fingerprint");
    assert_ne!(
        before, after,
        "schema version must be part of the snapshot-cache fingerprint (v4→v5 invalidation)"
    );
}

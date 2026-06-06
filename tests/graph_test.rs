//! Graph Query Tests
//!
//! Tests for graph traversal and query functionality.
//!
//! Ported from `__tests__/graph.test.ts`. The TS suite indexes real TS source
//! files through the CodeGraph facade; the facade/extraction pipeline is a
//! separate port task, so these tests build the equivalent graph fixture
//! directly via `QueryBuilder` (real SQLite, no mocks) — the node/edge shapes
//! mirror exactly what extraction + resolution produce for the TS test files
//! (base.ts / derived.ts / utils.ts / main.ts).

use std::rc::Rc;

use codegraph::db::{DatabaseConnection, QueryBuilder};
use codegraph::graph::{GraphQueryManager, GraphTraverser};
use codegraph::types::{
    Direction,
    Edge,
    EdgeKind,
    FileRecord,
    Language,
    Node,
    NodeKind,
    TraversalOptions,
};
use tempfile::TempDir;

// =============================================================================
// Fixture
// =============================================================================

// Node IDs (stable, readable — stand-ins for the hash IDs extraction creates).
const FILE_BASE: &str = "file:src/base.ts";
const FILE_DERIVED: &str = "file:src/derived.ts";
const FILE_UTILS: &str = "file:src/utils.ts";
const FILE_MAIN: &str = "file:src/main.ts";
const BASE_CLASS: &str = "class:BaseClass";
const GET_VALUE: &str = "method:BaseClass.getValue";
const VALUE_PROP: &str = "property:BaseClass.value";
const PRINTABLE: &str = "interface:Printable";
const DERIVED_CLASS: &str = "class:DerivedClass";
const PRINT: &str = "method:DerivedClass.print";
const GET_NAME: &str = "method:DerivedClass.getName";
const NAME_PROP: &str = "property:DerivedClass.name";
const FORMAT_VALUE: &str = "fn:formatValue";
const PROCESS_VALUE: &str = "fn:processValue";
const DOUBLE_VALUE: &str = "fn:doubleValue";
const UNUSED_HELPER: &str = "fn:unusedHelper";
const MAIN: &str = "fn:main";

struct Fixture {
    _dir: TempDir,
    _conn: DatabaseConnection,
    queries: Rc<QueryBuilder>,
}

impl Fixture {
    fn traverser(&self) -> GraphTraverser {
        GraphTraverser::new(Rc::clone(&self.queries))
    }

    fn manager(&self) -> GraphQueryManager {
        GraphQueryManager::new(Rc::clone(&self.queries))
    }
}

fn make_node(
    id: &str,
    kind: NodeKind,
    name: &str,
    qualified_name: &str,
    file_path: &str,
    exported: bool,
) -> Node {
    let mut node = Node::new(
        id,
        kind,
        name,
        qualified_name,
        file_path,
        Language::Typescript,
        1,
        10,
    );
    node.is_exported = Some(exported);
    node
}

fn make_file(path: &str) -> FileRecord {
    FileRecord {
        path: path.to_string(),
        content_hash: format!("hash-{path}"),
        language: Language::Typescript,
        size: 100,
        modified_at: 1_700_000_000_000,
        indexed_at: 1_700_000_000_000,
        node_count: 1,
        errors: None,
    }
}

/// Builds the graph the TS test fixture produces:
///
/// - `src/base.ts` — `BaseClass { value; getValue() }`, `interface Printable`
/// - `src/derived.ts` — `DerivedClass extends BaseClass implements Printable
///   { name; print(); getName() }`, imports from base
/// - `src/utils.ts` — `formatValue`, `processValue` (calls formatValue),
///   `doubleValue`, `unusedHelper` (dead code)
/// - `src/main.ts` — `main` (news up DerivedClass, calls processValue /
///   doubleValue / print / getValue), imports from derived + utils
fn build_fixture() -> Fixture {
    let dir = TempDir::new().expect("tempdir");
    let conn = DatabaseConnection::initialize(dir.path().join("codegraph.db")).expect("init db");
    let queries = Rc::new(QueryBuilder::new(conn.get_db().expect("db handle")));

    for path in [
        "src/base.ts",
        "src/derived.ts",
        "src/utils.ts",
        "src/main.ts",
    ] {
        queries.upsert_file(&make_file(path)).expect("upsert file");
    }

    let nodes = vec![
        // src/base.ts
        make_node(
            FILE_BASE,
            NodeKind::File,
            "base.ts",
            "src/base.ts",
            "src/base.ts",
            false,
        ),
        make_node(
            BASE_CLASS,
            NodeKind::Class,
            "BaseClass",
            "src/base.ts::BaseClass",
            "src/base.ts",
            true,
        ),
        make_node(
            GET_VALUE,
            NodeKind::Method,
            "getValue",
            "src/base.ts::BaseClass.getValue",
            "src/base.ts",
            false,
        ),
        make_node(
            VALUE_PROP,
            NodeKind::Property,
            "value",
            "src/base.ts::BaseClass.value",
            "src/base.ts",
            false,
        ),
        make_node(
            PRINTABLE,
            NodeKind::Interface,
            "Printable",
            "src/base.ts::Printable",
            "src/base.ts",
            true,
        ),
        // src/derived.ts
        make_node(
            FILE_DERIVED,
            NodeKind::File,
            "derived.ts",
            "src/derived.ts",
            "src/derived.ts",
            false,
        ),
        make_node(
            DERIVED_CLASS,
            NodeKind::Class,
            "DerivedClass",
            "src/derived.ts::DerivedClass",
            "src/derived.ts",
            true,
        ),
        make_node(
            PRINT,
            NodeKind::Method,
            "print",
            "src/derived.ts::DerivedClass.print",
            "src/derived.ts",
            false,
        ),
        make_node(
            GET_NAME,
            NodeKind::Method,
            "getName",
            "src/derived.ts::DerivedClass.getName",
            "src/derived.ts",
            false,
        ),
        make_node(
            NAME_PROP,
            NodeKind::Property,
            "name",
            "src/derived.ts::DerivedClass.name",
            "src/derived.ts",
            false,
        ),
        // src/utils.ts
        make_node(
            FILE_UTILS,
            NodeKind::File,
            "utils.ts",
            "src/utils.ts",
            "src/utils.ts",
            false,
        ),
        make_node(
            FORMAT_VALUE,
            NodeKind::Function,
            "formatValue",
            "src/utils.ts::formatValue",
            "src/utils.ts",
            true,
        ),
        make_node(
            PROCESS_VALUE,
            NodeKind::Function,
            "processValue",
            "src/utils.ts::processValue",
            "src/utils.ts",
            true,
        ),
        make_node(
            DOUBLE_VALUE,
            NodeKind::Function,
            "doubleValue",
            "src/utils.ts::doubleValue",
            "src/utils.ts",
            true,
        ),
        make_node(
            UNUSED_HELPER,
            NodeKind::Function,
            "unusedHelper",
            "src/utils.ts::unusedHelper",
            "src/utils.ts",
            false,
        ),
        // src/main.ts
        make_node(
            FILE_MAIN,
            NodeKind::File,
            "main.ts",
            "src/main.ts",
            "src/main.ts",
            false,
        ),
        make_node(
            MAIN,
            NodeKind::Function,
            "main",
            "src/main.ts::main",
            "src/main.ts",
            true,
        ),
    ];
    queries.insert_nodes(&nodes).expect("insert nodes");

    let edges = vec![
        // Containment
        Edge::new(FILE_BASE, BASE_CLASS, EdgeKind::Contains),
        Edge::new(FILE_BASE, PRINTABLE, EdgeKind::Contains),
        Edge::new(BASE_CLASS, GET_VALUE, EdgeKind::Contains),
        Edge::new(BASE_CLASS, VALUE_PROP, EdgeKind::Contains),
        Edge::new(FILE_DERIVED, DERIVED_CLASS, EdgeKind::Contains),
        Edge::new(DERIVED_CLASS, PRINT, EdgeKind::Contains),
        Edge::new(DERIVED_CLASS, GET_NAME, EdgeKind::Contains),
        Edge::new(DERIVED_CLASS, NAME_PROP, EdgeKind::Contains),
        Edge::new(FILE_UTILS, FORMAT_VALUE, EdgeKind::Contains),
        Edge::new(FILE_UTILS, PROCESS_VALUE, EdgeKind::Contains),
        Edge::new(FILE_UTILS, DOUBLE_VALUE, EdgeKind::Contains),
        Edge::new(FILE_UTILS, UNUSED_HELPER, EdgeKind::Contains),
        Edge::new(FILE_MAIN, MAIN, EdgeKind::Contains),
        // Imports (resolved to the imported symbols, as ReferenceResolver does)
        Edge::new(FILE_DERIVED, BASE_CLASS, EdgeKind::Imports),
        Edge::new(FILE_DERIVED, PRINTABLE, EdgeKind::Imports),
        Edge::new(FILE_MAIN, DERIVED_CLASS, EdgeKind::Imports),
        Edge::new(FILE_MAIN, PROCESS_VALUE, EdgeKind::Imports),
        Edge::new(FILE_MAIN, DOUBLE_VALUE, EdgeKind::Imports),
        // Type hierarchy
        Edge::new(DERIVED_CLASS, BASE_CLASS, EdgeKind::Extends),
        Edge::new(DERIVED_CLASS, PRINTABLE, EdgeKind::Implements),
        // Calls
        Edge::new(PROCESS_VALUE, FORMAT_VALUE, EdgeKind::Calls),
        Edge::new(PRINT, GET_NAME, EdgeKind::Calls),
        Edge::new(PRINT, GET_VALUE, EdgeKind::Calls),
        Edge::new(MAIN, PROCESS_VALUE, EdgeKind::Calls),
        Edge::new(MAIN, DOUBLE_VALUE, EdgeKind::Calls),
        Edge::new(MAIN, PRINT, EdgeKind::Calls),
        Edge::new(MAIN, GET_VALUE, EdgeKind::Calls),
        // Instantiation
        Edge::new(MAIN, DERIVED_CLASS, EdgeKind::Instantiates),
    ];
    queries.insert_edges(&edges).expect("insert edges");

    Fixture {
        _dir: dir,
        _conn: conn,
        queries,
    }
}

fn names(refs: &[codegraph::types::NodeRef]) -> Vec<String> {
    refs.iter().map(|r| r.node.name.clone()).collect()
}

// =============================================================================
// traverse()
// =============================================================================

#[test]
fn traverses_graph_from_a_starting_node() {
    let fx = build_fixture();
    let traverser = fx.traverser();

    let subgraph = traverser
        .traverse_bfs(
            MAIN,
            &TraversalOptions {
                max_depth: Some(2),
                direction: Some(Direction::Outgoing),
                ..Default::default()
            },
        )
        .unwrap();

    assert!(!subgraph.nodes.is_empty());
    assert!(subgraph.roots.contains(&MAIN.to_string()));
}

#[test]
fn respects_max_depth_option() {
    let fx = build_fixture();
    let traverser = fx.traverser();

    let shallow = traverser
        .traverse_bfs(
            MAIN,
            &TraversalOptions {
                max_depth: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
    let deep = traverser
        .traverse_bfs(
            MAIN,
            &TraversalOptions {
                max_depth: Some(3),
                ..Default::default()
            },
        )
        .unwrap();

    assert!(deep.nodes.len() >= shallow.nodes.len());
}

#[test]
fn supports_incoming_direction() {
    let fx = build_fixture();
    let traverser = fx.traverser();

    let subgraph = traverser
        .traverse_bfs(
            FORMAT_VALUE,
            &TraversalOptions {
                max_depth: Some(2),
                direction: Some(Direction::Incoming),
                ..Default::default()
            },
        )
        .unwrap();

    assert!(!subgraph.nodes.is_empty());
    // processValue calls formatValue, so it must appear upstream
    assert!(subgraph.nodes.contains_key(PROCESS_VALUE));
}

#[test]
fn returns_empty_subgraph_for_missing_start_node() {
    let fx = build_fixture();
    let traverser = fx.traverser();

    let subgraph = traverser
        .traverse_bfs("non-existent-id", &TraversalOptions::default())
        .unwrap();

    assert!(subgraph.nodes.is_empty());
    assert!(subgraph.edges.is_empty());
    assert!(subgraph.roots.is_empty());
}

#[test]
fn dfs_traversal_covers_outgoing_reachable_nodes() {
    let fx = build_fixture();
    let traverser = fx.traverser();

    let subgraph = traverser
        .traverse_dfs(
            FILE_MAIN,
            &TraversalOptions {
                direction: Some(Direction::Outgoing),
                ..Default::default()
            },
        )
        .unwrap();

    assert!(subgraph.nodes.contains_key(MAIN));
    assert!(subgraph.nodes.contains_key(PROCESS_VALUE));
    assert!(subgraph.roots.contains(&FILE_MAIN.to_string()));
}

// =============================================================================
// getContext()
// =============================================================================

#[test]
fn returns_context_for_a_node() {
    let fx = build_fixture();
    let manager = fx.manager();

    let context = manager.get_context(DERIVED_CLASS).unwrap();

    assert_eq!(context.focal.id, DERIVED_CLASS);
    // Ancestors: containing file
    assert!(context.ancestors.iter().any(|a| a.id == FILE_DERIVED));
    // Children: contained methods/properties
    let child_ids: Vec<&str> = context.children.iter().map(|c| c.id.as_str()).collect();
    assert!(child_ids.contains(&PRINT));
    assert!(child_ids.contains(&GET_NAME));
    // Incoming refs (non-contains): import from main.ts + instantiation by main
    assert!(
        context
            .incoming_refs
            .iter()
            .any(|r| r.node.id == MAIN && r.edge.kind == EdgeKind::Instantiates)
    );
    // Outgoing refs (non-contains): extends BaseClass, implements Printable
    assert!(
        context
            .outgoing_refs
            .iter()
            .any(|r| r.node.id == BASE_CLASS && r.edge.kind == EdgeKind::Extends)
    );
    assert!(
        context
            .outgoing_refs
            .iter()
            .any(|r| r.node.id == PRINTABLE && r.edge.kind == EdgeKind::Implements)
    );
    // Imports come from the containing file's import edges
    let import_ids: Vec<&str> = context.imports.iter().map(|i| i.id.as_str()).collect();
    assert!(import_ids.contains(&BASE_CLASS));
    assert!(import_ids.contains(&PRINTABLE));
}

#[test]
fn get_context_errors_for_non_existent_node() {
    let fx = build_fixture();
    let manager = fx.manager();

    let err = manager.get_context("non-existent-id").unwrap_err();
    assert!(
        err.to_string().contains("Node not found"),
        "unexpected error: {err}"
    );
}

// =============================================================================
// getCallGraph()
// =============================================================================

#[test]
fn returns_call_graph_for_a_function() {
    let fx = build_fixture();
    let traverser = fx.traverser();

    let call_graph = traverser.get_call_graph(PROCESS_VALUE, 2).unwrap();

    assert!(!call_graph.nodes.is_empty());
    assert!(call_graph.nodes.contains_key(PROCESS_VALUE));
    // Caller (main) and callee (formatValue) both included
    assert!(call_graph.nodes.contains_key(MAIN));
    assert!(call_graph.nodes.contains_key(FORMAT_VALUE));
}

// =============================================================================
// getTypeHierarchy()
// =============================================================================

#[test]
fn returns_type_hierarchy_for_a_class() {
    let fx = build_fixture();
    let traverser = fx.traverser();

    let hierarchy = traverser.get_type_hierarchy(DERIVED_CLASS).unwrap();

    assert!(!hierarchy.nodes.is_empty());
    assert!(hierarchy.nodes.contains_key(DERIVED_CLASS));
    // Ancestors via extends/implements
    assert!(hierarchy.nodes.contains_key(BASE_CLASS));
    assert!(hierarchy.nodes.contains_key(PRINTABLE));
}

#[test]
fn type_hierarchy_preserves_ts_quirk_descendants_not_traversed() {
    let fx = build_fixture();
    let traverser = fx.traverser();

    // Faithful-port quirk: in the TS source, getTypeAncestors and
    // getTypeDescendants share one `visited` set and ancestors runs first,
    // marking the focal node visited — so the descendants walk returns
    // immediately and incoming extends/implements edges are never followed.
    // DerivedClass (which extends BaseClass) is therefore NOT included.
    let hierarchy = traverser.get_type_hierarchy(BASE_CLASS).unwrap();
    assert!(hierarchy.nodes.contains_key(BASE_CLASS));
    assert!(!hierarchy.nodes.contains_key(DERIVED_CLASS));
}

#[test]
fn returns_empty_subgraph_for_non_existent_node() {
    let fx = build_fixture();
    let traverser = fx.traverser();

    let hierarchy = traverser.get_type_hierarchy("non-existent-id").unwrap();

    assert_eq!(hierarchy.nodes.len(), 0);
    assert_eq!(hierarchy.edges.len(), 0);
}

// =============================================================================
// findUsages()
// =============================================================================

#[test]
fn finds_usages_of_a_symbol() {
    let fx = build_fixture();
    let traverser = fx.traverser();

    let usages = traverser.find_usages(BASE_CLASS).unwrap();

    // Should find at least the extends relationship
    assert!(
        usages
            .iter()
            .any(|u| u.node.id == DERIVED_CLASS && u.edge.kind == EdgeKind::Extends)
    );
}

// =============================================================================
// getCallers() and getCallees()
// =============================================================================

#[test]
fn gets_callers_of_a_function() {
    let fx = build_fixture();
    let traverser = fx.traverser();

    let callers = traverser.get_callers(FORMAT_VALUE, 1).unwrap();

    // processValue calls formatValue
    assert!(names(&callers).contains(&"processValue".to_string()));
}

#[test]
fn gets_callees_of_a_function() {
    let fx = build_fixture();
    let traverser = fx.traverser();

    let callees = traverser.get_callees(PROCESS_VALUE, 1).unwrap();

    assert!(names(&callees).contains(&"formatValue".to_string()));
}

#[test]
fn traverses_callers_and_callees_by_batched_bfs_layers() {
    let fx = build_fixture();

    // Mirror the TS test's chain.ts:
    //   root() { middleA() + middleB() }  middleA/middleB() { leaf() }
    let chain_file = "src/chain.ts";
    fx.queries.upsert_file(&make_file(chain_file)).unwrap();
    let chain_nodes = vec![
        make_node(
            "file:src/chain.ts",
            NodeKind::File,
            "chain.ts",
            "src/chain.ts",
            chain_file,
            false,
        ),
        make_node(
            "fn:root",
            NodeKind::Function,
            "root",
            "src/chain.ts::root",
            chain_file,
            true,
        ),
        make_node(
            "fn:middleA",
            NodeKind::Function,
            "middleA",
            "src/chain.ts::middleA",
            chain_file,
            true,
        ),
        make_node(
            "fn:middleB",
            NodeKind::Function,
            "middleB",
            "src/chain.ts::middleB",
            chain_file,
            true,
        ),
        make_node(
            "fn:leaf",
            NodeKind::Function,
            "leaf",
            "src/chain.ts::leaf",
            chain_file,
            true,
        ),
    ];
    fx.queries.insert_nodes(&chain_nodes).unwrap();
    let chain_edges = vec![
        Edge::new("file:src/chain.ts", "fn:root", EdgeKind::Contains),
        Edge::new("file:src/chain.ts", "fn:middleA", EdgeKind::Contains),
        Edge::new("file:src/chain.ts", "fn:middleB", EdgeKind::Contains),
        Edge::new("file:src/chain.ts", "fn:leaf", EdgeKind::Contains),
        Edge::new("fn:root", "fn:middleA", EdgeKind::Calls),
        Edge::new("fn:root", "fn:middleB", EdgeKind::Calls),
        Edge::new("fn:middleA", "fn:leaf", EdgeKind::Calls),
        Edge::new("fn:middleB", "fn:leaf", EdgeKind::Calls),
    ];
    fx.queries.insert_edges(&chain_edges).unwrap();

    let traverser = fx.traverser();

    let callees = names(&traverser.get_callees("fn:root", 2).unwrap());
    assert!(callees.contains(&"middleA".to_string()));
    assert!(callees.contains(&"middleB".to_string()));
    assert!(callees.contains(&"leaf".to_string()));

    let callers = names(&traverser.get_callers("fn:leaf", 2).unwrap());
    assert!(callers.contains(&"middleA".to_string()));
    assert!(callers.contains(&"middleB".to_string()));
    assert!(callers.contains(&"root".to_string()));
}

// =============================================================================
// getImpactRadius()
// =============================================================================

#[test]
fn calculates_impact_radius() {
    let fx = build_fixture();
    let traverser = fx.traverser();

    let impact = traverser.get_impact_radius(FORMAT_VALUE, 3).unwrap();

    assert!(!impact.nodes.is_empty());
    assert!(impact.nodes.contains_key(FORMAT_VALUE));
    // processValue depends on formatValue; main depends on processValue
    assert!(impact.nodes.contains_key(PROCESS_VALUE));
    assert!(impact.nodes.contains_key(MAIN));
}

#[test]
fn does_not_drag_in_sibling_members_via_the_structural_contains_edge_536() {
    let fx = build_fixture();
    let traverser = fx.traverser();

    let impact = traverser.get_impact_radius(GET_NAME, 3).unwrap();
    // The containing class must NOT be pulled into impact just because it
    // *contains* getName — climbing that contains edge would re-expand every
    // sibling method and explode impact for a leaf symbol. (#536)
    assert!(!impact.nodes.contains_key(DERIVED_CLASS));
    // The actual dependent (print calls getName) IS included
    assert!(impact.nodes.contains_key(PRINT));
}

// =============================================================================
// findPath()
// =============================================================================

#[test]
fn finds_path_between_connected_nodes() {
    let fx = build_fixture();
    let traverser = fx.traverser();

    let path = traverser
        .find_path(PROCESS_VALUE, FORMAT_VALUE, &[])
        .unwrap();

    let path = path.expect("processValue -> formatValue path should exist");
    assert_eq!(path.len(), 2);
    assert_eq!(path[0].node.id, PROCESS_VALUE);
    assert!(path[0].edge.is_none());
    assert_eq!(path[1].node.id, FORMAT_VALUE);
    assert_eq!(path[1].edge.as_ref().map(|e| e.kind), Some(EdgeKind::Calls));
}

#[test]
fn returns_null_for_disconnected_nodes() {
    let fx = build_fixture();
    let traverser = fx.traverser();

    // Create two nodes that definitely don't have a path
    let path = traverser
        .find_path("non-existent-1", "non-existent-2", &[])
        .unwrap();

    assert!(path.is_none());
}

// =============================================================================
// getAncestors() and getChildren()
// =============================================================================

#[test]
fn gets_ancestors_of_a_node() {
    let fx = build_fixture();
    let traverser = fx.traverser();

    let ancestors = traverser.get_ancestors(PRINT).unwrap();

    // Should have class and file as ancestors (immediate parent first)
    let ids: Vec<&str> = ancestors.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(ids, vec![DERIVED_CLASS, FILE_DERIVED]);
}

#[test]
fn gets_children_of_a_node() {
    let fx = build_fixture();
    let traverser = fx.traverser();

    let children = traverser.get_children(DERIVED_CLASS).unwrap();

    // Should have methods as children
    let ids: Vec<&str> = children.iter().map(|c| c.id.as_str()).collect();
    assert!(ids.contains(&PRINT));
    assert!(ids.contains(&GET_NAME));
    assert!(ids.contains(&NAME_PROP));
}

// =============================================================================
// File dependency analysis
// =============================================================================

#[test]
fn gets_file_dependencies() {
    let fx = build_fixture();
    let manager = fx.manager();

    let deps = manager.get_file_dependencies("src/main.ts").unwrap();

    assert!(deps.contains(&"src/derived.ts".to_string()));
    assert!(deps.contains(&"src/utils.ts".to_string()));
    // Deduplicated: utils.ts appears once despite two imported symbols
    assert_eq!(deps.len(), 2);
}

#[test]
fn gets_file_dependents() {
    let fx = build_fixture();
    let manager = fx.manager();

    let dependents = manager.get_file_dependents("src/utils.ts").unwrap();

    assert_eq!(dependents, vec!["src/main.ts".to_string()]);
}

// =============================================================================
// findCircularDependencies()
// =============================================================================

#[test]
fn detects_circular_dependencies() {
    let fx = build_fixture();
    let manager = fx.manager();

    let cycles = manager.find_circular_dependencies().unwrap();

    // Our test files don't have circular deps
    assert!(cycles.is_empty());
}

#[test]
fn detects_an_actual_cycle() {
    let dir = TempDir::new().unwrap();
    let conn = DatabaseConnection::initialize(dir.path().join("codegraph.db")).unwrap();
    let queries = Rc::new(QueryBuilder::new(conn.get_db().unwrap()));

    for path in ["a.ts", "b.ts"] {
        queries.upsert_file(&make_file(path)).unwrap();
    }
    let nodes = vec![
        make_node("file:a.ts", NodeKind::File, "a.ts", "a.ts", "a.ts", false),
        make_node("fn:a", NodeKind::Function, "a", "a.ts::a", "a.ts", true),
        make_node("file:b.ts", NodeKind::File, "b.ts", "b.ts", "b.ts", false),
        make_node("fn:b", NodeKind::Function, "b", "b.ts::b", "b.ts", true),
    ];
    queries.insert_nodes(&nodes).unwrap();
    let edges = vec![
        Edge::new("file:a.ts", "fn:a", EdgeKind::Contains),
        Edge::new("file:b.ts", "fn:b", EdgeKind::Contains),
        Edge::new("file:a.ts", "fn:b", EdgeKind::Imports),
        Edge::new("file:b.ts", "fn:a", EdgeKind::Imports),
    ];
    queries.insert_edges(&edges).unwrap();

    let manager = GraphQueryManager::new(Rc::clone(&queries));
    let cycles = manager.find_circular_dependencies().unwrap();

    assert_eq!(cycles.len(), 1);
    let mut cycle = cycles[0].clone();
    cycle.sort();
    assert_eq!(cycle, vec!["a.ts".to_string(), "b.ts".to_string()]);
}

// =============================================================================
// findDeadCode()
// =============================================================================

#[test]
fn finds_dead_code() {
    let fx = build_fixture();
    let manager = fx.manager();

    let dead_code = manager.find_dead_code(Some(&[NodeKind::Function])).unwrap();

    // unusedHelper should be detected (not exported, no non-contains refs)
    assert!(dead_code.iter().any(|n| n.name == "unusedHelper"));
    // Exported functions are skipped even when unreferenced
    assert!(!dead_code.iter().any(|n| n.name == "doubleValue"));
}

// =============================================================================
// getNodeMetrics()
// =============================================================================

#[test]
fn returns_metrics_for_a_node() {
    let fx = build_fixture();
    let manager = fx.manager();

    let metrics = manager.get_node_metrics(PROCESS_VALUE).unwrap();

    // incoming: contains (file:utils), calls (main), imports (file:main)
    assert_eq!(metrics.incoming_edge_count, 3);
    // outgoing: calls formatValue
    assert_eq!(metrics.outgoing_edge_count, 1);
    assert_eq!(metrics.call_count, 1);
    assert_eq!(metrics.caller_count, 1);
    assert_eq!(metrics.child_count, 0);
    // ancestors: file:utils
    assert_eq!(metrics.depth, 1);
}

// =============================================================================
// Additional GraphQueryManager surface (not covered by graph.test.ts but part
// of src/graph/queries.ts — exported symbols, glob lookup, module structure,
// filtered subgraph)
// =============================================================================

#[test]
fn gets_exported_symbols_of_a_file() {
    let fx = build_fixture();
    let manager = fx.manager();

    let exported = manager.get_exported_symbols("src/utils.ts").unwrap();
    let names: Vec<&str> = exported.iter().map(|n| n.name.as_str()).collect();

    assert!(names.contains(&"formatValue"));
    assert!(names.contains(&"processValue"));
    assert!(names.contains(&"doubleValue"));
    assert!(!names.contains(&"unusedHelper"));
}

#[test]
fn finds_nodes_by_qualified_name_glob() {
    let fx = build_fixture();
    let manager = fx.manager();

    let matches = manager.find_by_qualified_name("src/utils.ts::*").unwrap();
    assert_eq!(matches.len(), 4); // all four utils functions

    let exact = manager
        .find_by_qualified_name("src/base.ts::BaseClass.getValue")
        .unwrap();
    assert_eq!(exact.len(), 1);
    assert_eq!(exact[0].id, GET_VALUE);

    // `?` matches a single character
    let single = manager.find_by_qualified_name("src/main.ts::mai?").unwrap();
    assert_eq!(single.len(), 1);
    assert_eq!(single[0].id, MAIN);
}

#[test]
fn gets_module_structure() {
    let fx = build_fixture();
    let manager = fx.manager();

    let structure = manager.get_module_structure().unwrap();

    let src = structure.get("src").expect("src directory present");
    assert_eq!(src.len(), 4);
    assert!(src.contains(&"src/main.ts".to_string()));
}

#[test]
fn gets_filtered_subgraph_with_edges() {
    let fx = build_fixture();
    let manager = fx.manager();

    let subgraph = manager
        .get_filtered_subgraph(|n| n.file_path == "src/utils.ts", true)
        .unwrap();

    // file node + 4 functions
    assert_eq!(subgraph.nodes.len(), 5);
    // Edges between matching nodes only (4 contains + processValue->formatValue)
    assert_eq!(subgraph.edges.len(), 5);
    assert!(subgraph.roots.is_empty());
}

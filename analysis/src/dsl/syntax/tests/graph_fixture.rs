use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::adapter::rust::RustAdapter;
use crate::builder::GraphBuilder;
use crate::edges::{EdgeData, EdgeKind};
use crate::graph::CodeGraph;
use crate::nodes::{NodeData, NodeId, NodeKind, Span, Visibility};

pub(super) fn span() -> Span {
    Span {
        file: PathBuf::from("t.rs"),
        start_line: 1,
        start_col: 0,
        end_line: 5,
        end_col: 1,
        byte_range: 0..50,
    }
}

pub(super) fn fixture_node(name: &str, kind: NodeKind) -> NodeData {
    NodeData {
        id: NodeId::new("t.rs", &format!("crate::{name}"), kind),
        kind,
        name: name.to_string(),
        qualified_name: format!("crate::{name}"),
        file_path: PathBuf::from("t.rs"),
        span: span(),
        visibility: Visibility::Public,
        metadata: HashMap::new(),
        birth_revision: 0,
        last_modified_revision: 0,
        complexity: None,
        cfg: None,
        dataflow: None,
    }
}

pub(super) fn fixture_calls_edge() -> EdgeData {
    fixture_edge(EdgeKind::Calls)
}

pub(super) fn fixture_edge(kind: EdgeKind) -> EdgeData {
    EdgeData {
        kind,
        source_span: span(),
        weight: 1.0,
    }
}

pub(super) fn build_sample_graph() -> CodeGraph {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let adapter = RustAdapter::new();
    GraphBuilder::build_from_files(&[fixtures.join("sample.rs")], &adapter)
}

pub(super) fn build_mutual_recursion_graph() -> CodeGraph {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let adapter = RustAdapter::new();
    GraphBuilder::build_from_files(&[fixtures.join("mutual_recursion.rs")], &adapter)
}

pub(super) fn build_deep_call_chain_graph() -> CodeGraph {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let adapter = RustAdapter::new();
    GraphBuilder::build_from_files(&[fixtures.join("deep_call_chain.rs")], &adapter)
}

pub(super) fn build_setalgebra_fixture() -> CodeGraph {
    let mut graph = CodeGraph::new();
    let a = graph.add_node(fixture_node("a", NodeKind::Function));
    let b = graph.add_node(fixture_node("b", NodeKind::Function));
    let c = graph.add_node(fixture_node("c", NodeKind::Function));
    let _d = graph.add_node(fixture_node("d", NodeKind::Function));
    graph.add_edge(&a, &b, fixture_calls_edge()).unwrap();
    graph.add_edge(&b, &c, fixture_calls_edge()).unwrap();
    let d_id = NodeId::new("t.rs", "crate::d", NodeKind::Function);
    graph
        .add_edge(
            &b,
            &d_id,
            fixture_edge(EdgeKind::UnresolvedCall("d".to_string())),
        )
        .unwrap();
    graph
}

pub(super) fn node_names(graph: &CodeGraph, ids: &[NodeId]) -> Vec<String> {
    ids.iter()
        .filter_map(|id| graph.get_node(id).map(|node| node.name.clone()))
        .collect()
}

pub(super) fn names_of(graph: &CodeGraph, ids: &[NodeId]) -> HashSet<String> {
    ids.iter()
        .filter_map(|id| graph.get_node(id).map(|node| node.name.clone()))
        .collect()
}

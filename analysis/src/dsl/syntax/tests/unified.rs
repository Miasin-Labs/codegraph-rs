use super::graph_fixture::fixture_node;
use crate::dsl::syntax::{QueryConfig, run_query_expr};
use crate::graph::CodeGraph;
use crate::nodes::NodeKind;

#[test]
fn unified_run_query_handles_aggregation_count() {
    let mut graph = CodeGraph::new();
    graph.add_node(fixture_node("foo", NodeKind::Function));
    let result = run_query_expr("count fn(\"foo\")", &graph, &QueryConfig::default()).unwrap();
    assert!(
        result
            .metadata
            .iter()
            .any(|line| line.starts_with("scalar = 1"))
    );
}

#[test]
fn unified_run_query_handles_exists() {
    let graph = CodeGraph::new();
    let result = run_query_expr("exists fn(\"missing\")", &graph, &QueryConfig::default()).unwrap();
    assert!(result.metadata.iter().any(|line| line == "bool = false"));
}

#[test]
fn unified_run_query_handles_legacy_pipe() {
    let mut graph = CodeGraph::new();
    graph.add_node(fixture_node("foo", NodeKind::Function));
    let result = run_query_expr("fn(\"foo\")", &graph, &QueryConfig::default()).unwrap();
    assert_eq!(result.nodes.len(), 1);
}

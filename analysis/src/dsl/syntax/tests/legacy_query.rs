use super::graph_fixture::{
    build_deep_call_chain_graph,
    build_mutual_recursion_graph,
    build_sample_graph,
    fixture_calls_edge,
    fixture_node,
    node_names,
};
use crate::dsl::syntax::{DslOp, QueryConfig, QueryEngine, run_query};
use crate::graph::CodeGraph;
use crate::nodes::NodeKind;

#[test]
fn test_query_preconditions_walks_callers_normal() {
    let mut graph = CodeGraph::new();
    let foo = graph.add_node(fixture_node("foo", NodeKind::Function));
    let bar = graph.add_node(fixture_node("bar", NodeKind::Function));
    let baz = graph.add_node(fixture_node("baz", NodeKind::Function));
    graph.add_edge(&foo, &bar, fixture_calls_edge()).unwrap();
    graph.add_edge(&bar, &baz, fixture_calls_edge()).unwrap();

    let ops = vec![DslOp::SelectFn("baz".into()), DslOp::Preconditions];
    let result = QueryEngine::new(&graph)
        .execute(&ops, &QueryConfig::default())
        .unwrap();
    let names: std::collections::HashSet<&str> = result
        .nodes
        .iter()
        .filter_map(|id| graph.get_node(id).map(|node| node.name.as_str()))
        .collect();
    assert!(names.contains("baz"));
    assert!(names.contains("bar"));
    assert!(names.contains("foo"));
}

#[test]
fn test_query_preconditions_terminates_on_cycle_robust() {
    let mut graph = CodeGraph::new();
    let ping = graph.add_node(fixture_node("ping", NodeKind::Function));
    let pong = graph.add_node(fixture_node("pong", NodeKind::Function));
    graph.add_edge(&ping, &pong, fixture_calls_edge()).unwrap();
    graph.add_edge(&pong, &ping, fixture_calls_edge()).unwrap();

    let ops = vec![DslOp::SelectFn("ping".into()), DslOp::Preconditions];
    let result = QueryEngine::new(&graph)
        .execute(&ops, &QueryConfig::default())
        .unwrap();
    assert!(
        result
            .cycles_detected
            .iter()
            .any(|id| *id == ping || *id == pong)
    );
    assert!(result.nodes.len() <= 2);
}

#[test]
fn test_query_fn_callees() {
    let graph = build_sample_graph();
    let result = run_query(r#"fn("foo") | callees"#, &graph, &QueryConfig::default()).unwrap();
    let names = node_names(&graph, &result.nodes);
    assert!(
        names.contains(&"bar".to_string()),
        "expected 'bar' in callees of foo, got: {names:?}"
    );
}

#[test]
fn test_query_fn_callers() {
    let graph = build_sample_graph();
    let result = run_query(r#"fn("baz") | callers"#, &graph, &QueryConfig::default()).unwrap();
    let names = node_names(&graph, &result.nodes);
    assert!(
        names.contains(&"bar".to_string()),
        "expected 'bar' in callers of baz, got: {names:?}"
    );
}

#[test]
fn test_query_depth() {
    let graph = build_sample_graph();
    let result = run_query(
        r#"fn("foo") | callees | depth 2"#,
        &graph,
        &QueryConfig::default(),
    )
    .unwrap();
    let names = node_names(&graph, &result.nodes);
    assert!(names.contains(&"bar".to_string()), "got: {names:?}");
    assert!(names.contains(&"baz".to_string()), "got: {names:?}");
}

#[test]
fn test_query_filter() {
    let graph = build_sample_graph();
    let result = run_query(
        r#"fn("foo") | callees | depth 3 | filter kind=Function"#,
        &graph,
        &QueryConfig::default(),
    )
    .unwrap();

    for id in &result.nodes {
        let node = graph.get_node(id).unwrap();
        assert_eq!(node.kind, NodeKind::Function);
    }
}

#[test]
fn test_query_type_select() {
    let graph = build_sample_graph();
    let result = run_query(r#"type("Config")"#, &graph, &QueryConfig::default()).unwrap();
    let names = node_names(&graph, &result.nodes);
    assert!(names.contains(&"Config".to_string()), "got: {names:?}");
    assert!(!result.nodes.is_empty());
}

#[test]
fn test_query_cycle_safe() {
    let graph = build_mutual_recursion_graph();
    let result = run_query(
        r#"fn("ping") | callees | depth 10"#,
        &graph,
        &QueryConfig::default(),
    )
    .unwrap();
    let names = node_names(&graph, &result.nodes);
    assert!(names.contains(&"pong".to_string()), "got: {names:?}");
    assert!(!result.cycles_detected.is_empty());
}

#[test]
fn test_query_max_nodes() {
    let graph = build_sample_graph();
    let config = QueryConfig {
        max_tokens: 4000,
        max_nodes: 2,
    };
    let result = run_query(r#"fn("foo") | callees | depth 5"#, &graph, &config).unwrap();
    if result.total_before_truncation > 2 {
        assert!(result.was_truncated);
        assert!(result.nodes.len() <= 2);
    }
}

#[test]
fn test_taint_basic() {
    let graph = build_deep_call_chain_graph();
    let config = QueryConfig {
        max_tokens: 4000,
        max_nodes: 50,
    };
    let result = run_query(r#"fn("a") | taint "x""#, &graph, &config).unwrap();
    let names = node_names(&graph, &result.nodes);
    for name in ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"] {
        assert!(names.contains(&name.to_string()), "got: {names:?}");
    }
    assert_eq!(result.nodes.len(), 10);
}

#[test]
fn test_taint_cycle_safe() {
    let graph = build_mutual_recursion_graph();
    let result = run_query(r#"fn("ping") | taint "n""#, &graph, &QueryConfig::default()).unwrap();
    let names = node_names(&graph, &result.nodes);
    assert!(names.contains(&"ping".to_string()), "got: {names:?}");
    assert!(names.contains(&"pong".to_string()), "got: {names:?}");
    assert!(!result.cycles_detected.is_empty());
}

#[test]
fn test_taint_respects_max_nodes() {
    let graph = build_deep_call_chain_graph();
    let config = QueryConfig {
        max_tokens: 4000,
        max_nodes: 3,
    };
    let result = run_query(r#"fn("a") | taint "x""#, &graph, &config).unwrap();
    assert!(result.nodes.len() <= 3);
}

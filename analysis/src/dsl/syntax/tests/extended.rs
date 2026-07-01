use super::graph_fixture::{build_setalgebra_fixture, fixture_edge, fixture_node, names_of};
use crate::dsl::syntax::{DslOp, QueryConfig, parse_expr, parse_query, run_query, run_query_expr};
use crate::edges::EdgeKind;
use crate::nodes::NodeKind;

#[test]
fn dsl_union_combines_results_normal() {
    let graph = build_setalgebra_fixture();
    let result = run_query_expr(
        r#"fn("a") | callees union fn("c")"#,
        &graph,
        &QueryConfig::default(),
    )
    .unwrap();
    let names = names_of(&graph, &result.nodes);
    assert!(names.contains("b"), "expected 'b' in union, got {names:?}");
    assert!(names.contains("c"), "expected 'c' in union, got {names:?}");
    assert!(result.edges.is_empty());
}

#[test]
fn dsl_intersect_keeps_only_common_normal() {
    let graph = build_setalgebra_fixture();
    let result = run_query_expr(
        r#"fn("a") | callees intersect fn("c") | callers"#,
        &graph,
        &QueryConfig::default(),
    )
    .unwrap();
    let names = names_of(&graph, &result.nodes);
    assert_eq!(names.len(), 1, "expected exactly {{b}}, got {names:?}");
    assert!(names.contains("b"));
}

#[test]
fn dsl_difference_subtracts_right_from_left_normal() {
    let graph = build_setalgebra_fixture();
    let result = run_query_expr(
        r#"fn("a") | callees | depth 1 \ fn("b")"#,
        &graph,
        &QueryConfig::default(),
    )
    .unwrap();
    let names = names_of(&graph, &result.nodes);
    assert!(!names.contains("b"), "b should be excluded, got {names:?}");
}

#[test]
fn dsl_path_finds_shortest_normal() {
    let graph = build_setalgebra_fixture();
    let result = run_query_expr(
        r#"path fn("a") -> fn("c")"#,
        &graph,
        &QueryConfig::default(),
    )
    .unwrap();
    let names = names_of(&graph, &result.nodes);
    assert!(names.contains("a"));
    assert!(names.contains("b"));
    assert!(names.contains("c"));
}

#[test]
fn dsl_paths_returns_all_simple_paths_normal() {
    let graph = build_setalgebra_fixture();
    let result = run_query_expr(
        r#"paths fn("a") -> fn("c") depth 5"#,
        &graph,
        &QueryConfig::default(),
    )
    .unwrap();
    let names = names_of(&graph, &result.nodes);
    assert_eq!(names.len(), 3);
    assert!(names.contains("a") && names.contains("b") && names.contains("c"));
}

#[test]
fn dsl_path_via_edge_kind_filters_normal() {
    let graph = build_setalgebra_fixture();
    let positive = run_query_expr(
        r#"paths fn("a") -> fn("d") via UnresolvedCall depth 5"#,
        &graph,
        &QueryConfig::default(),
    )
    .unwrap();
    let names = names_of(&graph, &positive.nodes);
    assert!(names.contains("a") && names.contains("b") && names.contains("d"));

    let negative = run_query_expr(
        r#"paths fn("a") -> fn("c") via UnresolvedCall depth 5"#,
        &graph,
        &QueryConfig::default(),
    )
    .unwrap();
    assert!(negative.nodes.is_empty());
}

#[test]
fn dsl_path_via_extended_edge_kind_matches_normal() {
    let mut graph = crate::graph::CodeGraph::new();
    let child = graph.add_node(fixture_node("child", NodeKind::Function));
    let parent = graph.add_node(fixture_node("parent", NodeKind::Function));
    graph
        .add_edge(&child, &parent, fixture_edge(EdgeKind::Extends))
        .unwrap();

    let result = run_query_expr(
        r#"path fn("child") -> fn("parent") via Extends"#,
        &graph,
        &QueryConfig::default(),
    )
    .unwrap();

    let names = names_of(&graph, &result.nodes);
    assert_eq!(names.len(), 2, "got {names:?}");
    assert!(names.contains("child"));
    assert!(names.contains("parent"));
}

#[test]
fn dsl_entrypoints_returns_classified_normal() {
    let graph = build_setalgebra_fixture();
    let result = run_query_expr("entrypoints", &graph, &QueryConfig::default()).unwrap();
    let names = names_of(&graph, &result.nodes);
    assert_eq!(names.len(), 4, "got {names:?}");
    assert!(!result.metadata.is_empty());
    for line in &result.metadata {
        assert!(line.starts_with("PublicApi "), "got {line}");
    }

    let result_main =
        run_query_expr("entrypoints kind=Main", &graph, &QueryConfig::default()).unwrap();
    assert!(result_main.nodes.is_empty());
    let result_pub = run_query_expr(
        "entrypoints kind=PublicApi",
        &graph,
        &QueryConfig::default(),
    )
    .unwrap();
    assert_eq!(result_pub.nodes.len(), 4);
}

#[test]
fn dsl_entrypoints_pipe_runs_normal_pipe_ops() {
    let graph = build_setalgebra_fixture();
    let callers = run_query_expr("entrypoints | callers", &graph, &QueryConfig::default()).unwrap();
    let caller_names = names_of(&graph, &callers.nodes);
    assert!(caller_names.contains("a"), "got {caller_names:?}");
    assert!(caller_names.contains("b"), "got {caller_names:?}");

    let depth = run_query_expr("entrypoints | depth 2", &graph, &QueryConfig::default()).unwrap();
    let depth_names = names_of(&graph, &depth.nodes);
    assert!(depth_names.contains("c"), "got {depth_names:?}");
}

#[test]
fn dsl_parens_unbalanced_robust() {
    let err = parse_expr(r#"(fn("a") union fn("b")"#).unwrap_err();
    assert!(err.message.contains(')'), "msg = {}", err.message);
}

#[test]
fn dsl_since_filters_old_nodes_normal() {
    let mut graph = crate::graph::CodeGraph::new();
    let _old = graph.add_node(fixture_node("old_fn", NodeKind::Function));
    let cutoff = graph.current_revision() + 1;
    let _new = graph.add_node(fixture_node("new_fn", NodeKind::Function));

    let parsed = parse_query(&format!(r#"fn("fn") | since {cutoff}"#)).unwrap();
    assert!(matches!(parsed.last(), Some(DslOp::Since(rev)) if *rev == cutoff));

    let result = run_query(
        &format!(r#"fn("fn") | since {cutoff}"#),
        &graph,
        &QueryConfig::default(),
    )
    .unwrap();
    let names: std::collections::HashSet<&str> = result
        .nodes
        .iter()
        .filter_map(|id| graph.get_node(id).map(|node| node.name.as_str()))
        .collect();
    assert!(names.contains("new_fn"), "got: {names:?}");
    assert!(!names.contains("old_fn"), "got: {names:?}");
}

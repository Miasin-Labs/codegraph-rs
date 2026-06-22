use super::graph_fixture::{fixture_calls_edge, fixture_edge, fixture_node, names_of};
use crate::dsl::syntax::{QueryConfig, run_query, run_query_expr};
use crate::edges::EdgeKind;
use crate::nodes::NodeKind;

#[test]
fn dsl_reachable_via_constrains_label_sequence_normal() {
    let mut graph = crate::graph::CodeGraph::new();
    let module = graph.add_node(fixture_node("m", NodeKind::Module));
    let f0 = graph.add_node(fixture_node("f0", NodeKind::Function));
    let f1 = graph.add_node(fixture_node("f1", NodeKind::Function));
    let f2 = graph.add_node(fixture_node("f2", NodeKind::Function));
    let struct_id = graph.add_node(fixture_node("S", NodeKind::Struct));
    graph
        .add_edge(&module, &f0, fixture_edge(EdgeKind::Contains))
        .unwrap();
    graph.add_edge(&f0, &f1, fixture_calls_edge()).unwrap();
    graph.add_edge(&f1, &f2, fixture_calls_edge()).unwrap();
    graph
        .add_edge(&f1, &struct_id, fixture_edge(EdgeKind::UsesType))
        .unwrap();

    let result = run_query(
        r#"fn("f0") | reachable via "Calls+""#,
        &graph,
        &QueryConfig::default(),
    )
    .unwrap();
    let names = names_of(&graph, &result.nodes);
    assert!(
        names.contains("f1") && names.contains("f2"),
        "got {names:?}"
    );
    assert!(!names.contains("S"), "UsesType target leaked: {names:?}");
    assert!(!names.contains("f0"), "Plus requires >= 1 edge: {names:?}");
    assert!(
        result
            .metadata
            .iter()
            .any(|line| line.contains("reachable via \"Calls+\"") && line.contains("seeds=1")),
        "got {:?}",
        result.metadata
    );

    let result = run_query(
        r#"type("S") | reachable via "UsesType" incoming"#,
        &graph,
        &QueryConfig::default(),
    )
    .unwrap();
    let names = names_of(&graph, &result.nodes);
    assert_eq!(
        names,
        std::collections::HashSet::from(["f1".to_string()]),
        "got {names:?}"
    );
}

#[test]
fn dsl_reachable_via_incoming_and_empty_seed_robust() {
    let mut graph = crate::graph::CodeGraph::new();
    let caller = graph.add_node(fixture_node("caller", NodeKind::Function));
    let mid = graph.add_node(fixture_node("mid", NodeKind::Function));
    let target = graph.add_node(fixture_node("target", NodeKind::Function));
    graph.add_edge(&caller, &mid, fixture_calls_edge()).unwrap();
    graph.add_edge(&mid, &target, fixture_calls_edge()).unwrap();

    let result = run_query(
        r#"fn("target") | reachable via "Calls+" incoming"#,
        &graph,
        &QueryConfig::default(),
    )
    .unwrap();
    let names = names_of(&graph, &result.nodes);
    assert!(
        names.contains("caller") && names.contains("mid"),
        "got {names:?}"
    );

    let result = run_query(r#"reachable via "Calls+""#, &graph, &QueryConfig::default()).unwrap();
    assert!(result.nodes.is_empty());
    assert!(
        result
            .metadata
            .iter()
            .any(|line| line.contains("empty working set")),
        "got {:?}",
        result.metadata
    );
}

#[test]
fn dsl_reachable_via_composes_with_extended_grammar_robust() {
    let mut graph = crate::graph::CodeGraph::new();
    let a = graph.add_node(fixture_node("a", NodeKind::Function));
    let b = graph.add_node(fixture_node("b", NodeKind::Function));
    let c = graph.add_node(fixture_node("c", NodeKind::Function));
    graph.add_edge(&a, &b, fixture_calls_edge()).unwrap();
    graph.add_edge(&b, &c, fixture_calls_edge()).unwrap();

    let result = run_query_expr(
        r#"(fn("a") | reachable via "Calls+") diff fn("b")"#,
        &graph,
        &QueryConfig::default(),
    )
    .unwrap();
    let names = names_of(&graph, &result.nodes);
    assert_eq!(
        names,
        std::collections::HashSet::from(["c".to_string()]),
        "got {names:?}"
    );

    let result = run_query_expr(
        r#"paths fn("a") -> fn("c") via Calls depth 5"#,
        &graph,
        &QueryConfig::default(),
    )
    .unwrap();
    let names = names_of(&graph, &result.nodes);
    assert!(names.contains("a") && names.contains("c"), "got {names:?}");
}

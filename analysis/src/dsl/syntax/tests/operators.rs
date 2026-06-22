use super::graph_fixture::{fixture_calls_edge, fixture_edge, fixture_node, names_of};
use crate::dsl::syntax::{DslOp, QueryConfig, parse_query, run_query, run_query_expr};
use crate::edges::EdgeKind;
use crate::nodes::NodeKind;

#[test]
fn dsl_hot_bare_returns_top_n_normal() {
    let mut graph = crate::graph::CodeGraph::new();
    let a = graph.add_node(fixture_node("a", NodeKind::Function));
    let b = graph.add_node(fixture_node("b", NodeKind::Function));
    let c = graph.add_node(fixture_node("c", NodeKind::Function));
    graph.add_edge(&a, &b, fixture_calls_edge()).unwrap();
    graph.add_edge(&a, &c, fixture_calls_edge()).unwrap();
    graph.add_edge(&c, &b, fixture_calls_edge()).unwrap();

    let parsed = parse_query("hot 2").unwrap();
    assert_eq!(parsed, vec![DslOp::Hot(2)]);
    let result = run_query("hot 2", &graph, &QueryConfig::default()).unwrap();
    assert!(result.nodes.len() <= 2);
    assert!(!result.metadata.is_empty());
}

#[test]
fn dsl_hot_postfix_reranks_working_set_robust() {
    let mut graph = crate::graph::CodeGraph::new();
    let a = graph.add_node(fixture_node("a", NodeKind::Function));
    let b = graph.add_node(fixture_node("b", NodeKind::Function));
    let c = graph.add_node(fixture_node("c", NodeKind::Function));
    graph.add_edge(&a, &b, fixture_calls_edge()).unwrap();
    graph.add_edge(&a, &c, fixture_calls_edge()).unwrap();
    let result = run_query(
        r#"fn("a") | callees | hot 1"#,
        &graph,
        &QueryConfig::default(),
    )
    .unwrap();
    assert!(result.nodes.len() <= 1);
}

#[test]
fn dsl_scc_bare_returns_cycle_members_normal() {
    let mut graph = crate::graph::CodeGraph::new();
    let ping = graph.add_node(fixture_node("ping", NodeKind::Function));
    let pong = graph.add_node(fixture_node("pong", NodeKind::Function));
    let _isolated = graph.add_node(fixture_node("isolated", NodeKind::Function));
    graph.add_edge(&ping, &pong, fixture_calls_edge()).unwrap();
    graph.add_edge(&pong, &ping, fixture_calls_edge()).unwrap();

    let parsed = parse_query("scc").unwrap();
    assert_eq!(parsed, vec![DslOp::Scc]);
    let result = run_query("scc", &graph, &QueryConfig::default()).unwrap();
    let names = names_of(&graph, &result.nodes);
    assert!(names.contains("ping") && names.contains("pong"));
    assert!(!names.contains("isolated"));
    assert!(result.metadata.iter().any(|line| line.contains("size=2")));
}

#[test]
fn dsl_dominators_of_walks_chain_normal() {
    let mut graph = crate::graph::CodeGraph::new();
    let main_n = graph.add_node(fixture_node("main", NodeKind::Function));
    let mid = graph.add_node(fixture_node("mid", NodeKind::Function));
    let leaf = graph.add_node(fixture_node("leaf", NodeKind::Function));
    graph.add_edge(&main_n, &mid, fixture_calls_edge()).unwrap();
    graph.add_edge(&mid, &leaf, fixture_calls_edge()).unwrap();

    let result = run_query_expr(
        r#"dominators of fn("leaf")"#,
        &graph,
        &QueryConfig::default(),
    )
    .unwrap();
    let names = names_of(&graph, &result.nodes);
    assert!(names.contains("main"), "got {names:?}");
    assert!(names.contains("mid"), "got {names:?}");
    assert!(!names.contains("leaf"));
}

#[test]
fn dsl_dominates_returns_descendants_robust() {
    let mut graph = crate::graph::CodeGraph::new();
    let main_n = graph.add_node(fixture_node("main", NodeKind::Function));
    let mid = graph.add_node(fixture_node("mid", NodeKind::Function));
    let leaf = graph.add_node(fixture_node("leaf", NodeKind::Function));
    graph.add_edge(&main_n, &mid, fixture_calls_edge()).unwrap();
    graph.add_edge(&mid, &leaf, fixture_calls_edge()).unwrap();

    let result =
        run_query_expr(r#"dominates fn("main")"#, &graph, &QueryConfig::default()).unwrap();
    let names = names_of(&graph, &result.nodes);
    assert!(names.contains("mid"), "got {names:?}");
    assert!(names.contains("leaf"), "got {names:?}");
}

#[test]
fn dsl_trait_impls_returns_implementors_normal() {
    let mut graph = crate::graph::CodeGraph::new();
    let trait_id = graph.add_node(fixture_node("MyTrait", NodeKind::Trait));
    let first = graph.add_node(fixture_node("Foo", NodeKind::Struct));
    let second = graph.add_node(fixture_node("Bar", NodeKind::Struct));
    graph
        .add_edge(&first, &trait_id, fixture_edge(EdgeKind::Implements))
        .unwrap();
    graph
        .add_edge(&second, &trait_id, fixture_edge(EdgeKind::Implements))
        .unwrap();

    let result = run_query_expr(
        r#"trait_impls of type("MyTrait")"#,
        &graph,
        &QueryConfig::default(),
    )
    .unwrap();
    let names = names_of(&graph, &result.nodes);
    assert!(names.contains("Foo"));
    assert!(names.contains("Bar"));
    assert!(
        result
            .metadata
            .iter()
            .any(|line| line.contains("trait_impls"))
    );
}

#[test]
fn dsl_cluster_by_type_emits_metadata_normal() {
    let mut graph = crate::graph::CodeGraph::new();
    let config_type = graph.add_node(fixture_node("Config", NodeKind::Struct));
    let load = graph.add_node(fixture_node("load", NodeKind::Function));
    let save = graph.add_node(fixture_node("save", NodeKind::Function));
    graph
        .add_edge(&load, &config_type, fixture_edge(EdgeKind::UsesType))
        .unwrap();
    graph
        .add_edge(&save, &config_type, fixture_edge(EdgeKind::UsesType))
        .unwrap();

    let parsed = parse_query("cluster by type").unwrap();
    assert_eq!(parsed, vec![DslOp::ClusterByType]);
    let result = run_query("cluster by type", &graph, &QueryConfig::default()).unwrap();
    let names = names_of(&graph, &result.nodes);
    assert!(names.contains("load"));
    assert!(names.contains("save"));
    assert!(result.metadata.iter().any(|line| line.contains("Config")));
}

#[test]
fn dsl_affected_returns_changed_neighborhood_normal() {
    let mut graph = crate::graph::CodeGraph::new();
    let _a = graph.add_node(fixture_node("a", NodeKind::Function));
    let cutoff = graph.current_revision() + 1;
    let b = graph.add_node(fixture_node("b", NodeKind::Function));
    let c = graph.add_node(fixture_node("c", NodeKind::Function));
    graph.add_edge(&b, &c, fixture_calls_edge()).unwrap();

    let parsed = parse_query(&format!("affected 1 since {cutoff}")).unwrap();
    assert_eq!(
        parsed,
        vec![DslOp::Affected {
            depth: 1,
            since_rev: cutoff
        }]
    );
    let result = run_query(
        &format!("affected 1 since {cutoff}"),
        &graph,
        &QueryConfig::default(),
    )
    .unwrap();
    let names = names_of(&graph, &result.nodes);
    assert!(names.contains("b"), "got {names:?}");
    assert!(names.contains("c"), "got {names:?}");
}

#[test]
fn dsl_multi_path_picks_shortest_from_any_source_robust() {
    let mut graph = crate::graph::CodeGraph::new();
    let a = graph.add_node(fixture_node("a", NodeKind::Function));
    let b = graph.add_node(fixture_node("b", NodeKind::Function));
    let c = graph.add_node(fixture_node("c", NodeKind::Function));
    let x = graph.add_node(fixture_node("x", NodeKind::Function));
    let y = graph.add_node(fixture_node("y", NodeKind::Function));
    let z = graph.add_node(fixture_node("z", NodeKind::Function));
    graph.add_edge(&a, &b, fixture_calls_edge()).unwrap();
    graph.add_edge(&b, &c, fixture_calls_edge()).unwrap();
    graph.add_edge(&x, &y, fixture_calls_edge()).unwrap();
    graph.add_edge(&y, &z, fixture_calls_edge()).unwrap();
    graph.add_edge(&z, &c, fixture_calls_edge()).unwrap();

    let result = run_query_expr(
        r#"multi_path { fn("a"), fn("x") } -> fn("c") depth 5"#,
        &graph,
        &QueryConfig::default(),
    )
    .unwrap();
    let names = names_of(&graph, &result.nodes);
    assert!(names.contains("a"));
    assert!(names.contains("b"));
    assert!(names.contains("c"));
    assert!(!names.contains("y"), "got {names:?}");
}

#[test]
fn dsl_dispatch_filter_restricts_to_trait_callers_robust() {
    let mut graph = crate::graph::CodeGraph::new();
    let trait_id = graph.add_node(fixture_node("Iter", NodeKind::Trait));
    let trait_method = graph.add_node(fixture_node("next", NodeKind::Function));
    let caller = graph.add_node(fixture_node("user", NodeKind::Function));
    let _other = graph.add_node(fixture_node("other_fn", NodeKind::Function));
    graph
        .add_edge(&trait_id, &trait_method, fixture_edge(EdgeKind::Contains))
        .unwrap();
    graph
        .add_edge(&caller, &trait_method, fixture_calls_edge())
        .unwrap();

    let parsed = parse_query("dispatch").unwrap();
    assert_eq!(parsed, vec![DslOp::Dispatch]);
    let result = run_query("dispatch", &graph, &QueryConfig::default()).unwrap();
    let names = names_of(&graph, &result.nodes);
    assert!(names.contains("user"), "got {names:?}");
    assert!(!names.contains("other_fn"), "got {names:?}");
}

use super::*;

#[test]
fn centrality_and_critical_run_over_call_fixture() {
    let (graph, _a, _b, _c) = fixture();

    let centrality = centrality_report(&graph, 3);
    assert_eq!(centrality.analyzed, 5);
    assert_eq!(centrality.nodes.len(), 3, "--top caps the listing");
    // Scores are sorted descending.
    for pair in centrality.nodes.windows(2) {
        assert!(pair[0].score >= pair[1].score);
    }

    // b is the only path between a and c → articulation node.
    let critical = critical_report(&graph, 10);
    assert!(
        critical.nodes.iter().any(|n| n.name == "b"),
        "b articulates a-c: {critical:?}"
    );
    assert!(critical.bridge_count >= 1, "a->b / b->c are bridges");
}

#[test]
fn export_report_emits_dot_for_graph_and_subgraph() {
    let (graph, a, _b, _c) = fixture();

    let whole = export_report(&graph, None, 0).unwrap();
    assert_eq!(whole.scope, "graph");
    assert_eq!(whole.node_count, 5);
    assert!(whole.dot.starts_with("digraph"));
    assert!(whole.dot.contains("calls"));

    let sub = export_report(&graph, Some(&a), 1).unwrap();
    assert_eq!(sub.scope, "subgraph");
    assert!(sub.node_count >= 2, "a plus its 1-hop neighborhood");
    assert!(sub.dot.starts_with("digraph"));
}

#[test]
fn taint_suggest_ranks_named_sources_and_sinks() {
    let mut graph = AnalysisGraph::new();
    let read = add_fn(&mut graph, "src/io.ts", "readUserInput", 1);
    let exec = add_fn(&mut graph, "src/db.ts", "execQuery", 1);
    let plain = add_fn(&mut graph, "src/x.ts", "tally", 1);
    add_call(&mut graph, &read, &exec, "src/io.ts");
    let _ = plain;

    let report = taint_suggest_report(&graph, 10);
    assert_eq!(report.source_count, 1);
    assert_eq!(report.sink_count, 1);
    assert_eq!(report.sources[0].symbol.name, "readUserInput");
    assert_eq!(report.sinks[0].symbol.name, "execQuery");
    assert_eq!(report.pairs.len(), 1);
    assert!(report.pairs[0].priority > 0.0);

    // No lexicon match at all → honest note, no panic.
    let mut empty = AnalysisGraph::new();
    add_fn(&mut empty, "src/x.ts", "tally", 1);
    let report = taint_suggest_report(&empty, 10);
    assert_eq!(report.pairs.len(), 0);
    assert!(report.note.contains("nothing to rank"));
}

#[test]
fn boundaries_report_is_honestly_empty_over_metadata_free_graph() {
    let (mut graph, _, _, _) = fixture();
    let report = boundaries_report(&mut graph);
    assert_eq!(report.boundary_count, 0);
    assert!(report.note.contains("does not populate these keys"));
    assert_eq!(report.cross_language_calls.edges_emitted, 0);
}

#[test]
fn capabilities_report_lists_all_six_with_cascades() {
    let report = capabilities_report();
    assert_eq!(report.capabilities.len(), 6);
    let validation = report
        .capabilities
        .iter()
        .find(|c| c.name == "virtualValidation")
        .unwrap();
    assert_eq!(
        validation.env_var,
        "CODEGRAPH_ANALYSIS_CAP_VIRTUAL_VALIDATION"
    );
    let call_graph = report
        .capabilities
        .iter()
        .find(|c| c.name == "callGraph")
        .unwrap();
    assert!(
        call_graph
            .disables
            .contains(&"virtualValidation".to_string()),
        "disabling callGraph cascades: {call_graph:?}"
    );
}

#[test]
fn schema_text_returns_engine_schemas_and_rejects_unknown() {
    for kind in [
        "query_result",
        "entrypoint_summary",
        "context_result",
        "formatted_output",
    ] {
        let schema = schema_text(kind).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(schema).unwrap();
        assert!(parsed["title"].is_string(), "{kind} schema parses");
    }
    // Hyphens and case are normalized.
    assert!(schema_text("Query-Result").is_ok());

    let err = schema_text("bogus").unwrap_err();
    assert!(err.contains("known kinds"));
}

#[test]
fn stats_report_counts_kinds_and_exact_reachability() {
    let (graph, _a, _b, _c) = fixture();
    let report = stats_report(&graph, true, 10);
    assert_eq!(report.node_count, 5);
    assert_eq!(report.nodes_by_kind.get("function"), Some(&5));
    assert_eq!(report.edges_by_kind.get("calls"), Some(&4));
    assert_eq!(report.file_count, 2);
    assert_eq!(report.placeholder_count, 0);

    let reach = report.reachability.expect("requested");
    assert_eq!(reach.method, "exact", "small graph gets exact numbers");
    let a_entry = reach
        .top
        .iter()
        .find(|e| e.symbol.name == "a")
        .expect("a listed");
    assert_eq!(a_entry.descendants, 2.0, "a reaches b and c");
    assert_eq!(a_entry.ancestors, 0.0);
    // d/e form a 2-cycle: each reaches the other.
    let d_entry = reach.top.iter().find(|e| e.symbol.name == "d").unwrap();
    assert_eq!(d_entry.descendants, 1.0);
    assert_eq!(d_entry.ancestors, 1.0);
}

#[test]
fn co_change_report_is_honest_without_git_history() {
    let (graph, _a, _b, _c) = fixture();
    let tmp = tempfile::tempdir().unwrap();
    // Not a git repository → zero commits, honest note, exit-0 shape.
    let report = co_change_report(&graph, tmp.path(), None, 2, 100, 10);
    assert_eq!(report.commits_analyzed, 0);
    assert!(report.pairs.is_empty());
    assert!(report.note.contains("No git history"));
}

#[test]
fn signature_type_params_extracts_generic_tokens() {
    assert_eq!(signature_type_params("(x: T) -> T"), vec!["T"]);
    assert_eq!(
        signature_type_params("(map: HashMap<K, V>) -> V"),
        vec!["K", "V"]
    );
    assert!(signature_type_params("(x: number): number").is_empty());
    assert!(signature_type_params("(s: &str) -> String").is_empty());
}

#[test]
fn generics_report_reports_metadata_gap_honestly() {
    let (graph, _a, _b, _c) = fixture();
    let report = generics_report(&graph, None);
    assert_eq!(report.instantiation_count, 0);
    assert!(report.note.contains("does not populate"));
}

#[test]
fn types_report_propagates_via_pass_manager() {
    let (mut graph, a, _b, _c) = fixture();
    let report = types_report(&mut graph, &a).unwrap().unwrap();
    assert_eq!(report.symbol.name, "a");
    // Fixture has no UsesType edges — honest empty, not an error.
    assert!(report.input_types.is_empty());
    assert!(report.note.contains("No concrete types"));
}

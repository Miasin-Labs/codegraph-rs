use super::{
    REPORT_SCHEMA_VERSION,
    ReportEnvelope,
    explain_report,
    fixture,
    query_report,
    validate_report,
};

#[test]
fn query_report_runs_pipe_dsl_over_call_edges() {
    let (graph, _a, _b, _c) = fixture();
    let report = query_report(&graph, r#"fn("a") | callees"#, 50, false).unwrap();
    assert_eq!(report.node_count, 1, "a's only callee is b");
    assert_eq!(report.nodes[0].name, "b");
    assert!(!report.truncated);
    assert!(report.why.is_none(), "why is opt-in");
}

#[test]
fn query_report_why_records_seed_predecessor() {
    let (graph, _a, _b, _c) = fixture();
    let report = query_report(&graph, r#"fn("a") | callees"#, 50, true).unwrap();
    let why = report.why.expect("pipe queries are traceable");
    let entry = why
        .iter()
        .find(|w| w.symbol.name == "b")
        .expect("result node b is explained");
    assert!(
        entry
            .steps
            .iter()
            .any(|s| s.predecessors.iter().any(|p| p == "a")),
        "b's provenance references seed a: {why:?}"
    );
}

#[test]
fn query_report_parse_error_quotes_bad_token() {
    let (graph, _, _, _) = fixture();
    let err = query_report(&graph, r#"fn("a") | bogus_op"#, 50, false).unwrap_err();
    assert!(err.contains("bogus_op"), "offending token quoted: {err}");
    assert!(err.contains("position"), "position included: {err}");
}

#[test]
fn explain_report_fuses_depth_without_executing() {
    let report = explain_report(r#"fn("a") | callees | callees | callees"#).unwrap();
    assert_eq!(report.kind, "pipe");
    assert!(
        report.steps.iter().any(|s| s.contains("Depth(3)")),
        "depth fusion applied: {:?}",
        report.steps
    );
}

#[test]
fn explain_report_classifies_aggregations_and_rejects_bad_queries() {
    let agg = explain_report(r#"count fn("a")"#).unwrap();
    assert_eq!(agg.kind, "aggregation");

    let err = explain_report(r#"fn("a") | bogus_op"#).unwrap_err();
    assert!(err.contains("bogus_op"), "offending token quoted: {err}");
}

#[test]
fn report_envelope_serializes_camel_case_wire_shape() {
    let envelope = ReportEnvelope::new("cycles", serde_json::json!({"cycleCount": 1}));
    let value = serde_json::to_value(&envelope).unwrap();
    assert_eq!(value["schemaVersion"], REPORT_SCHEMA_VERSION);
    assert_eq!(value["kind"], "cycles");
    assert_eq!(value["data"]["cycleCount"], 1);
}

#[test]
fn validate_report_judges_arity_changes_per_caller() {
    let (graph, _a, b, _c) = fixture();

    // Arity change: every direct caller flagged incompatible.
    let changed = validate_report(&graph, &b, 1, 2).unwrap();
    assert!(!changed.is_safe);
    assert_eq!(changed.incompatible.len(), 1, "only a calls b");
    assert_eq!(changed.incompatible[0].symbol.name, "a");
    assert_eq!(changed.call_sites.len(), 1);

    // Unchanged arity: safe, callers compatible.
    let unchanged = validate_report(&graph, &b, 2, 2).unwrap();
    assert!(unchanged.is_safe);
    assert_eq!(unchanged.compatible.len(), 1);
    assert!(unchanged.note.contains("call-graph"));
}

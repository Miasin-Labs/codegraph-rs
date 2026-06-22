use super::{
    AnalysisGraph,
    HashMap,
    StoredComplexity,
    add_call,
    add_fn_span,
    base_snapshot,
    diff_report,
    fixture,
};

#[test]
fn diff_report_finds_added_removed_changed_and_impact() {
    // Base: a → b → c, plus doomed.
    let mut base = AnalysisGraph::new();
    let a = add_fn_span(&mut base, "src/x.ts", "a", 1, 3);
    let b = add_fn_span(&mut base, "src/x.ts", "b", 5, 8);
    let c = add_fn_span(&mut base, "src/x.ts", "c", 10, 12);
    add_call(&mut base, &a, &b, "src/x.ts");
    add_call(&mut base, &b, &c, "src/x.ts");
    let doomed = add_fn_span(&mut base, "src/y.ts", "doomed", 1, 2);
    add_call(&mut base, &a, &doomed, "src/x.ts");

    // Current: b grew (5..8 → 5..11), c/a shifted but same length,
    // doomed removed, fresh added (called by c).
    let mut current = AnalysisGraph::new();
    let a2 = add_fn_span(&mut current, "src/x.ts", "a", 1, 3);
    let b2 = add_fn_span(&mut current, "src/x.ts", "b", 5, 11);
    let c2 = add_fn_span(&mut current, "src/x.ts", "c", 13, 15);
    add_call(&mut current, &a2, &b2, "src/x.ts");
    add_call(&mut current, &b2, &c2, "src/x.ts");
    let fresh = add_fn_span(&mut current, "src/x.ts", "fresh", 17, 18);
    add_call(&mut current, &c2, &fresh, "src/x.ts");

    let base = base_snapshot(base);
    let current_complexity = HashMap::from([(
        b2.clone(),
        StoredComplexity {
            cyclomatic: 4,
            cognitive: 3,
            max_nesting: 2,
        },
    )]);
    let report = diff_report(&base, &current, &current_complexity, 3, 50);

    assert_eq!(report.nodes_added_count, 1);
    assert_eq!(report.nodes_added[0].name, "fresh");
    assert_eq!(report.nodes_removed_count, 1);
    assert_eq!(report.nodes_removed[0].name, "doomed");
    // Exactly b changed — c moved but kept its length.
    assert_eq!(report.nodes_changed_count, 1);
    assert_eq!(report.nodes_changed[0].symbol.name, "b");
    assert_eq!(report.nodes_changed[0].reasons, vec!["spanLines"]);

    // Edge delta: a→doomed gone, c→fresh new.
    assert_eq!(report.edges_removed_count, 1);
    assert_eq!(report.edges_removed[0].to, "doomed");
    assert_eq!(report.edges_added_count, 1);
    assert_eq!(report.edges_added[0].from, "c");
    assert_eq!(report.edges_added[0].kind, "calls");

    // Changed/added functions carry complexity: after measured, before
    // honestly absent (no sidecar in the base).
    let b_delta = report
        .changed_functions
        .iter()
        .find(|f| f.symbol.name == "b")
        .expect("b listed");
    assert_eq!(b_delta.cyclomatic_after, Some(4));
    assert_eq!(b_delta.cyclomatic_before, None);
    assert_eq!(b_delta.cyclomatic_delta, None);
    assert_eq!(b_delta.lines_before, 4);
    assert_eq!(b_delta.lines_after, 7);
    assert!(report.note.contains("Base complexity is unavailable"));

    // Impact: b changed → a (its caller); fresh added → c, b, a;
    // doomed removed → a (in the base graph).
    let impacted: Vec<&str> = report
        .impact
        .nodes
        .iter()
        .map(|n| n.name.as_str())
        .collect();
    assert!(impacted.contains(&"a"), "callers impacted: {impacted:?}");
    assert!(impacted.contains(&"c"), "fresh's caller impacted");
    assert!(
        !impacted.contains(&"b") && !impacted.contains(&"fresh"),
        "delta nodes are not their own impact: {impacted:?}"
    );
    assert!(report.new_cycles.is_empty());
    assert_eq!(report.base.source, "cache-prev");
}

#[test]
fn diff_report_complexity_delta_when_base_sidecar_present() {
    let mut base_graph = AnalysisGraph::new();
    let b = add_fn_span(&mut base_graph, "src/x.ts", "b", 5, 8);
    let mut base = base_snapshot(base_graph);
    base.complexity.insert(
        b.clone(),
        StoredComplexity {
            cyclomatic: 2,
            cognitive: 1,
            max_nesting: 1,
        },
    );

    let mut current = AnalysisGraph::new();
    let b2 = add_fn_span(&mut current, "src/x.ts", "b", 5, 11);
    let current_complexity = HashMap::from([(
        b2,
        StoredComplexity {
            cyclomatic: 5,
            cognitive: 4,
            max_nesting: 2,
        },
    )]);
    let report = diff_report(&base, &current, &current_complexity, 3, 50);
    let delta = &report.changed_functions[0];
    assert_eq!(delta.cyclomatic_before, Some(2));
    assert_eq!(delta.cyclomatic_after, Some(5));
    assert_eq!(delta.cyclomatic_delta, Some(3));
    assert_eq!(delta.cognitive_delta, Some(3));
    assert!(!report.note.contains("Base complexity is unavailable"));
}

#[test]
fn diff_report_surfaces_newly_introduced_cycles_only() {
    // Base already has d ↔ e; current adds g ↔ h.
    let mut base_graph = AnalysisGraph::new();
    let d = add_fn_span(&mut base_graph, "src/y.ts", "d", 1, 2);
    let e = add_fn_span(&mut base_graph, "src/y.ts", "e", 4, 5);
    add_call(&mut base_graph, &d, &e, "src/y.ts");
    add_call(&mut base_graph, &e, &d, "src/y.ts");

    let mut current = AnalysisGraph::new();
    let d2 = add_fn_span(&mut current, "src/y.ts", "d", 1, 2);
    let e2 = add_fn_span(&mut current, "src/y.ts", "e", 4, 5);
    add_call(&mut current, &d2, &e2, "src/y.ts");
    add_call(&mut current, &e2, &d2, "src/y.ts");
    let g = add_fn_span(&mut current, "src/z.ts", "g", 1, 2);
    let h = add_fn_span(&mut current, "src/z.ts", "h", 4, 5);
    add_call(&mut current, &g, &h, "src/z.ts");
    add_call(&mut current, &h, &g, "src/z.ts");

    let report = diff_report(&base_snapshot(base_graph), &current, &HashMap::new(), 3, 50);
    assert_eq!(report.new_cycle_count, 1, "only g↔h is new");
    let names: Vec<&str> = report.new_cycles[0]
        .members
        .iter()
        .map(|m| m.name.as_str())
        .collect();
    assert_eq!(names, vec!["g", "h"]);
    assert_eq!(report.resolved_cycle_count, 0);
}

#[test]
fn diff_report_is_empty_for_identical_graphs() {
    let (graph, _a, _b, _c) = fixture();
    let (same, _, _, _) = fixture();
    let report = diff_report(&base_snapshot(same), &graph, &HashMap::new(), 3, 50);
    assert_eq!(report.nodes_added_count, 0);
    assert_eq!(report.nodes_removed_count, 0);
    assert_eq!(report.nodes_changed_count, 0);
    assert_eq!(report.edges_added_count, 0);
    assert_eq!(report.edges_removed_count, 0);
    assert!(report.changed_functions.is_empty());
    assert_eq!(report.impact.impacted_count, 0);
}

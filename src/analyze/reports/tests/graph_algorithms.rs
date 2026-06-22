use super::{
    SliceDirection,
    communities_report,
    cycles_report,
    dominators_report,
    fixture,
    impact_report,
    slice_report,
    taint_report,
};

#[test]
fn forward_slice_walks_callees_and_backward_walks_callers() {
    let (graph, a, _b, c) = fixture();

    let fwd = slice_report(&graph, &a, SliceDirection::Forward, 10).unwrap();
    assert_eq!(fwd.direction, "forward");
    assert_eq!(fwd.size, 2, "a influences b and c");
    assert!(fwd.nodes.iter().any(|n| n.name == "c"));

    let bwd = slice_report(&graph, &c, SliceDirection::Backward, 10).unwrap();
    assert_eq!(bwd.size, 2, "c is affected by a and b");
    assert!(bwd.nodes.iter().any(|n| n.name == "a"));
    assert!(bwd.note.contains("call-graph"));
}

#[test]
fn cycles_report_finds_mutual_recursion_only() {
    let (graph, _, _, _) = fixture();
    let report = cycles_report(&graph);
    assert_eq!(report.cycle_count, 1);
    assert_eq!(report.cycles[0].kind, "mutualRecursion");
    let names: Vec<&str> = report.cycles[0]
        .members
        .iter()
        .map(|m| m.name.as_str())
        .collect();
    assert_eq!(names, vec!["d", "e"]);
    assert_eq!(report.break_suggestions.len(), 1);
}

#[test]
fn dominators_chain_back_to_entry() {
    let (graph, a, b, c) = fixture();
    let report = dominators_report(&graph, &a, 50).unwrap();
    assert_eq!(report.entry.name, "a");
    assert_eq!(report.analyzed, 2);
    let c_entry = report
        .nodes
        .iter()
        .find(|n| n.symbol.name == "c")
        .expect("c analyzed");
    assert_eq!(
        c_entry
            .immediate_dominator
            .as_ref()
            .map(|s| s.name.as_str()),
        Some("b"),
        "every path from a to c passes through b"
    );
    assert_eq!(c_entry.dominator_depth, 2);
    let _ = (b, c);
}

#[test]
fn taint_report_annotates_call_hops() {
    let (graph, a, _b, c) = fixture();
    let report = taint_report(&graph, &a, &c, 8, 25).unwrap();
    assert_eq!(report.path_count, 1);
    let path = &report.paths[0];
    let names: Vec<&str> = path.nodes.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(names, vec!["a", "b", "c"]);
    assert_eq!(path.edge_kinds, vec!["calls", "calls"]);
    assert!(report.note.contains("dataflow IR"));
}

#[test]
fn impact_report_groups_call_sites_by_file() {
    let (graph, _a, b, _c) = fixture();
    let report = impact_report(&graph, &b, Some("fn b(x: i32)")).unwrap();
    assert_eq!(report.new_signature, "fn b(x: i32)");
    assert_eq!(report.call_site_count, 1, "only a calls b");
    assert_eq!(report.tasks.len(), 1);
    assert_eq!(report.tasks[0].call_sites[0].caller, "a");
}

#[test]
fn communities_report_is_deterministic_and_groups_call_clusters() {
    let (graph, _, _, _) = fixture();
    let one = communities_report(&graph, 8);
    let two = communities_report(&graph, 8);
    assert_eq!(one.community_count, two.community_count);
    assert_eq!(one.multi_member_count, two.multi_member_count);
    assert!(one.multi_member_count >= 1, "call clusters detected");
}

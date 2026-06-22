use std::collections::HashMap;
use std::path::PathBuf;

use super::{render_context, render_explore, render_impact, render_search_results};
use crate::context::budget::ExploreBudget;
use crate::context::heuristics::TaskIntent;
use crate::edges::{EdgeData, EdgeKind};
use crate::graph::CodeGraph;
use crate::nodes::{NodeData, NodeId, NodeKind, Span, Visibility};

fn span(start: u32) -> Span {
    Span {
        file: PathBuf::from("src/lib.rs"),
        start_line: start,
        start_col: 0,
        end_line: start + 1,
        end_col: 0,
        byte_range: 0..1,
    }
}

fn node(name: &str, kind: NodeKind, line: u32) -> NodeData {
    let id = NodeId::new("src/lib.rs", &format!("crate::{name}"), kind);
    NodeData {
        id,
        kind,
        name: name.to_string(),
        qualified_name: format!("crate::{name}"),
        file_path: PathBuf::from("src/lib.rs"),
        span: span(line),
        visibility: Visibility::Public,
        metadata: HashMap::new(),
        birth_revision: 0,
        last_modified_revision: 0,
        complexity: None,
        cfg: None,
        dataflow: None,
    }
}

#[test]
fn render_search_results_includes_kind_and_signature() {
    let mut g = CodeGraph::new();
    let id = g.add_node(node("foo", NodeKind::Function, 10));
    let out = render_search_results(&g, None, "foo", &[id], None);
    assert!(out.contains("## Search Results"));
    assert!(out.contains("foo (function)"));
    assert!(out.contains("`fn foo(...)`"));
    assert!(out.contains(":10"));
}

#[test]
fn render_search_empty_states_no_results() {
    let g = CodeGraph::new();
    let out = render_search_results(&g, None, "missing", &[], None);
    assert!(out.contains("No results"));
}

#[test]
fn render_impact_groups_by_file() {
    let mut g = CodeGraph::new();
    let a = g.add_node(node("a", NodeKind::Function, 1));
    let b = g.add_node(node("b", NodeKind::Function, 5));
    let out = render_impact(&g, "ToolCall", &[a, b], None);
    assert!(out.contains("## Impact"));
    assert!(out.contains("affects 2 symbols"));
    assert!(out.contains("**src/lib.rs:**"));
    assert!(out.contains("crate::a:1, crate::b:5"));
}

#[test]
fn render_context_includes_feature_reminder() {
    let mut g = CodeGraph::new();
    let id = g.add_node(node("foo", NodeKind::Function, 1));
    let budget = ExploreBudget::for_file_count(1000);
    let out = render_context(
        &g,
        "add a thing",
        &[id],
        &[],
        &[],
        TaskIntent::Feature,
        &budget,
    );
    assert!(out.contains("UX preferences"));
}

#[test]
fn render_context_omits_reminder_for_bugs() {
    let mut g = CodeGraph::new();
    let id = g.add_node(node("foo", NodeKind::Function, 1));
    let budget = ExploreBudget::for_file_count(1000);
    let out = render_context(
        &g,
        "fix the foo crash",
        &[id],
        &[],
        &[],
        TaskIntent::Bug,
        &budget,
    );
    assert!(!out.contains("UX preferences"));
}

#[test]
fn render_context_includes_relationship_map() {
    let mut g = CodeGraph::new();
    let caller = g.add_node(node("caller", NodeKind::Function, 1));
    let callee = g.add_node(node("callee", NodeKind::Function, 10));
    g.add_edge(
        &caller,
        &callee,
        EdgeData {
            kind: EdgeKind::Calls,
            source_span: span(3),
            weight: 1.0,
        },
    )
    .unwrap();
    let budget = ExploreBudget::for_file_count(1000);
    let out = render_context(
        &g,
        "caller",
        &[caller],
        &[callee],
        &[],
        TaskIntent::Exploration,
        &budget,
    );
    assert!(out.contains("### Relationships"));
    assert!(out.contains("**calls:**"));
    assert!(out.contains("crate::caller:1 -> crate::callee:10"));
}

#[test]
fn render_context_uses_qualified_symbol_labels() {
    let mut g = CodeGraph::new();
    let id = g.add_node(node("foo", NodeKind::Function, 7));
    let budget = ExploreBudget::for_file_count(1000);
    let included_blocks = [(id, "fn foo() {}\n".to_string())];
    let out = render_context(
        &g,
        "foo",
        std::slice::from_ref(&included_blocks[0].0),
        &[],
        &included_blocks,
        TaskIntent::Exploration,
        &budget,
    );
    assert!(out.contains("**crate::foo**"));
    assert!(out.contains("#### crate::foo (src/lib.rs:7)"));
}

#[test]
fn render_explore_keeps_included_blocks_intact() {
    let mut budget = ExploreBudget::for_file_count(1000);
    budget.max_output_chars = 80;
    let body = "line\n".repeat(100);
    let out = render_explore(
        "test",
        1,
        1,
        &[],
        &[(
            "src/lib.rs".to_string(),
            "rust".to_string(),
            "crate::long_fn(function)".to_string(),
            body.clone(),
        )],
        &[],
        &[],
        &[],
        &budget,
    );
    assert!(out.contains(&body));
    assert!(!out.contains("output truncated"));
}

#[test]
fn render_explore_emits_relationships() {
    let budget = ExploreBudget::for_file_count(1000);
    let rels = vec![(EdgeKind::Calls, vec![("a".to_string(), "b".to_string())])];
    let out = render_explore("test", 1, 1, &rels, &[], &[], &[], &[], &budget);
    assert!(out.contains("### Relationships"));
    assert!(out.contains("**calls:**"));
    assert!(out.contains("a → b"));
}

use std::collections::{HashMap, HashSet};

use super::labels::{edge_kind_label, relation_label};
use crate::context::budget::ExploreBudget;
use crate::edges::EdgeKind;
use crate::graph::CodeGraph;
use crate::nodes::NodeId;

pub(super) fn push_relationships(
    out: &mut String,
    graph: &CodeGraph,
    entry_points: &[NodeId],
    related: &[NodeId],
    budget: &ExploreBudget,
) {
    if !budget.include_relationships {
        return;
    }
    let node_set: HashSet<NodeId> = entry_points.iter().chain(related.iter()).cloned().collect();
    if node_set.len() < 2 {
        return;
    }

    let mut by_kind: HashMap<EdgeKind, Vec<(String, String)>> = HashMap::new();
    for src in &node_set {
        let Some(src_node) = graph.get_node(src) else {
            continue;
        };
        for (target, edge) in graph.get_edges_from(src) {
            if !node_set.contains(target) || !is_context_relationship_edge(&edge.kind) {
                continue;
            }
            let Some(target_node) = graph.get_node(target) else {
                continue;
            };
            let src_label = relation_label(src_node);
            let target_label = relation_label(target_node);
            let entries = by_kind.entry(edge.kind.clone()).or_default();
            if !entries
                .iter()
                .any(|(s, t)| s == &src_label && t == &target_label)
            {
                entries.push((src_label, target_label));
            }
        }
    }

    if by_kind.is_empty() {
        return;
    }
    let mut rels: Vec<_> = by_kind.into_iter().collect();
    rels.sort_by_key(|(kind, edges)| {
        (
            edge_kind_label(kind).to_string(),
            std::cmp::Reverse(edges.len()),
        )
    });

    out.push_str("### Relationships\n\n");
    for (kind, edges) in rels {
        out.push_str(&format!("**{}:**\n", edge_kind_label(&kind)));
        for (src, target) in edges.iter().take(budget.max_edges_per_relationship_kind) {
            out.push_str(&format!("- {src} -> {target}\n"));
        }
        if edges.len() > budget.max_edges_per_relationship_kind {
            out.push_str(&format!(
                "- ... and {} more\n",
                edges.len() - budget.max_edges_per_relationship_kind
            ));
        }
        out.push('\n');
    }
}

fn is_context_relationship_edge(kind: &EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::Calls
            | EdgeKind::UnresolvedCall(_)
            | EdgeKind::UsesType
            | EdgeKind::References
            | EdgeKind::Implements
            | EdgeKind::Returns
            | EdgeKind::TypeOf
    )
}

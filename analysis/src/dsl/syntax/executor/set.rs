use std::collections::HashSet;

use super::super::SetOp;
use super::QueryResult;
use crate::nodes::NodeId;

/// Combine two [`QueryResult`]s under a [`SetOp`]. Only `nodes` are preserved.
pub(super) fn combine_set_op(
    op: SetOp,
    left: QueryResult,
    right: QueryResult,
    max_nodes: usize,
) -> QueryResult {
    let left_nodes: HashSet<NodeId> = left.nodes.into_iter().collect();
    let right_nodes: HashSet<NodeId> = right.nodes.into_iter().collect();
    let merged: Vec<NodeId> = match op {
        SetOp::Union => left_nodes.union(&right_nodes).cloned().collect(),
        SetOp::Intersect => left_nodes.intersection(&right_nodes).cloned().collect(),
        SetOp::Diff => left_nodes.difference(&right_nodes).cloned().collect(),
    };
    let total = merged.len();
    let was_truncated = total > max_nodes;
    let nodes = if was_truncated {
        merged[..max_nodes].to_vec()
    } else {
        merged
    };
    QueryResult {
        nodes,
        edges: Vec::new(),
        was_truncated,
        total_before_truncation: total,
        cycles_detected: Vec::new(),
        metadata: Vec::new(),
    }
}

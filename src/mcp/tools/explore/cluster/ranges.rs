use std::collections::HashSet;

use super::super::super::format::{OrderedNodeMap, display_symbol};
use super::ClusterRequest;
use crate::error::Result;
use crate::types::{EdgeKind, NodeKind};

pub(super) struct LineRange {
    pub(super) start: i64,
    pub(super) end: i64,
    pub(super) name: String,
    pub(super) kind: String,
    pub(super) importance: i64,
}

pub(super) fn collect_ranges(req: &ClusterRequest<'_>) -> Result<Vec<LineRange>> {
    let mut range_nodes = OrderedNodeMap::new();
    for n in &req.group.nodes {
        if n.start_line > 0 && n.end_line > 0 {
            range_nodes.insert(n.clone());
        }
    }
    for id in &req.flow.named_node_ids {
        if range_nodes.contains(id) {
            continue;
        }
        if let Some(n) = req.cg.get_node(id)? {
            if n.file_path == req.file_path && n.start_line > 0 && n.end_line > 0 {
                range_nodes.insert(n);
            }
        }
    }

    let mut ranges: Vec<LineRange> = range_nodes
        .values()
        .filter(|n| {
            !(envelope_kind(n.kind)
                && (n.end_line as i64 - n.start_line as i64 + 1) as f64
                    > req.file_lines.len() as f64 * 0.5)
        })
        .map(|n| LineRange {
            start: n.start_line as i64,
            end: n.end_line as i64,
            name: display_symbol(n),
            kind: n.kind.as_str().to_string(),
            importance: range_importance(req, n.id.as_str()),
        })
        .collect();

    let mut edge_lines = HashSet::new();
    for node in &req.group.nodes {
        for edge in req.cg.get_outgoing_edges(&node.id)? {
            let Some(line) = edge.line else { continue };
            if line == 0 || edge.kind == EdgeKind::Contains {
                continue;
            }
            let key = format!("{}:{}", line, edge.target);
            if !edge_lines.insert(key) {
                continue;
            }
            let target_name = req
                .nodes
                .get(&edge.target)
                .map(display_symbol)
                .unwrap_or_else(|| edge.kind.as_str().to_string());
            ranges.push(LineRange {
                start: line as i64,
                end: line as i64,
                name: target_name,
                kind: edge.kind.as_str().to_string(),
                importance: 2,
            });
        }
    }
    Ok(ranges)
}

fn envelope_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::File
            | NodeKind::Module
            | NodeKind::Class
            | NodeKind::Struct
            | NodeKind::Interface
            | NodeKind::Enum
            | NodeKind::Namespace
            | NodeKind::Protocol
            | NodeKind::Trait
            | NodeKind::Component
    )
}

fn range_importance(req: &ClusterRequest<'_>, node_id: &str) -> i64 {
    if req.entry_node_ids.contains(node_id) {
        10
    } else if req.flow.named_node_ids.contains(node_id) {
        9
    } else if req.glue_node_ids.contains(node_id) {
        6
    } else if req.connected_to_entry.contains(node_id) {
        3
    } else {
        1
    }
}

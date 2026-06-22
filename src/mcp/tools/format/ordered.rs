//! Ordered graph context containers shared by graph and explore tools.

use std::collections::{HashMap, HashSet};

use crate::types::Node;

pub(in crate::mcp::tools) struct OrderedNodeMap {
    order: Vec<String>,
    map: HashMap<String, Node>,
}

impl OrderedNodeMap {
    pub(in crate::mcp::tools) fn new() -> Self {
        OrderedNodeMap {
            order: Vec::new(),
            map: HashMap::new(),
        }
    }

    pub(in crate::mcp::tools) fn contains(&self, id: &str) -> bool {
        self.map.contains_key(id)
    }

    pub(in crate::mcp::tools) fn get(&self, id: &str) -> Option<&Node> {
        self.map.get(id)
    }

    pub(in crate::mcp::tools) fn insert(&mut self, node: Node) {
        if !self.map.contains_key(&node.id) {
            self.order.push(node.id.clone());
        }
        self.map.insert(node.id.clone(), node);
    }

    pub(in crate::mcp::tools) fn values(&self) -> impl Iterator<Item = &Node> {
        self.order.iter().filter_map(|id| self.map.get(id))
    }

    pub(in crate::mcp::tools) fn keys(&self) -> impl Iterator<Item = &String> {
        self.order.iter()
    }

    pub(in crate::mcp::tools) fn len(&self) -> usize {
        self.order.len()
    }
}

/// Deterministic ordering for a `Subgraph`'s nodes. TS Maps preserve the
/// builder's insertion order; Rust's `Subgraph.nodes` is a `HashMap`, so we
/// impose roots-first (in `roots` order) then (filePath, startLine, name, id).
/// Tie ordering downstream may differ from TS in unpinned cases — see
/// notes/mcp-tools.md.
pub(in crate::mcp::tools) fn ordered_nodes_from_subgraph(
    sg: &crate::types::Subgraph,
) -> OrderedNodeMap {
    let mut out = OrderedNodeMap::new();
    for id in &sg.roots {
        if let Some(n) = sg.nodes.get(id) {
            out.insert(n.clone());
        }
    }
    let mut rest: Vec<&Node> = sg.nodes.values().filter(|n| !out.contains(&n.id)).collect();
    rest.sort_by(|a, b| {
        a.file_path
            .cmp(&b.file_path)
            .then(a.start_line.cmp(&b.start_line))
            .then(a.name.cmp(&b.name))
            .then(a.id.cmp(&b.id))
    });
    for n in rest {
        out.insert(n.clone());
    }
    out
}

/// Result of `build_flow_from_named_symbols`.
pub(in crate::mcp::tools) struct FlowInfo {
    pub(in crate::mcp::tools) text: String,
    pub(in crate::mcp::tools) path_node_ids: HashSet<String>,
    pub(in crate::mcp::tools) named_node_ids: HashSet<String>,
    pub(in crate::mcp::tools) unique_named_node_ids: HashSet<String>,
}

impl FlowInfo {
    pub(in crate::mcp::tools) fn empty() -> Self {
        FlowInfo {
            text: String::new(),
            path_node_ids: HashSet::new(),
            named_node_ids: HashSet::new(),
            unique_named_node_ids: HashSet::new(),
        }
    }
}

pub(in crate::mcp::tools) struct SynthNote {
    #[allow(dead_code)]
    pub(in crate::mcp::tools) label: String,
    pub(in crate::mcp::tools) compact: String,
    #[allow(dead_code)]
    pub(in crate::mcp::tools) registered_at: Option<String>,
}

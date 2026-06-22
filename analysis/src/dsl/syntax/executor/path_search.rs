use std::collections::{HashMap, HashSet};

use crate::graph::CodeGraph;
use crate::nodes::NodeId;

pub(super) fn all_simple_paths_bounded(
    graph: &CodeGraph,
    from: &NodeId,
    to: &NodeId,
    max_depth: usize,
    node_budget: usize,
) -> Vec<Vec<NodeId>> {
    let mut search = SimplePathSearch {
        graph,
        target: to,
        max_depth,
        node_budget,
        results: Vec::new(),
        total_nodes_emitted: HashSet::new(),
    };
    let mut stack = vec![from.clone()];
    let mut on_stack = HashMap::new();
    on_stack.insert(from.clone(), ());
    search.dfs(from, &mut stack, &mut on_stack);
    search.results
}

struct SimplePathSearch<'a> {
    graph: &'a CodeGraph,
    target: &'a NodeId,
    max_depth: usize,
    node_budget: usize,
    results: Vec<Vec<NodeId>>,
    total_nodes_emitted: HashSet<NodeId>,
}

impl<'a> SimplePathSearch<'a> {
    fn dfs(
        &mut self,
        current: &NodeId,
        stack: &mut Vec<NodeId>,
        on_stack: &mut HashMap<NodeId, ()>,
    ) {
        crate::ensure_sufficient_stack(|| self.dfs_inner(current, stack, on_stack));
    }

    fn dfs_inner(
        &mut self,
        current: &NodeId,
        stack: &mut Vec<NodeId>,
        on_stack: &mut HashMap<NodeId, ()>,
    ) {
        if stack.len() > self.max_depth + 1 {
            return;
        }
        if current == self.target {
            for node in stack.iter() {
                self.total_nodes_emitted.insert(node.clone());
            }
            self.results.push(stack.clone());
            return;
        }
        if self.total_nodes_emitted.len() >= self.node_budget {
            return;
        }
        for (next, _) in self.graph.get_edges_from(current) {
            if on_stack.contains_key(next) {
                continue;
            }
            stack.push(next.clone());
            on_stack.insert(next.clone(), ());
            self.dfs(next, stack, on_stack);
            on_stack.remove(next);
            stack.pop();
            if self.total_nodes_emitted.len() >= self.node_budget {
                return;
            }
        }
    }
}

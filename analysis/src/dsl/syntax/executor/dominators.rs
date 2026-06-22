use std::collections::HashSet;

use super::{QueryConfig, QueryEngine, QueryResult};
use crate::nodes::{NodeId, NodeKind};

impl<'a> QueryEngine<'a> {
    /// Walk the dominator chain from each seed up to the root.
    pub(super) fn execute_dominators_of(
        &self,
        seeds: &[NodeId],
        config: &QueryConfig,
    ) -> QueryResult {
        let mut result = QueryResult::default();
        let Some((root_id, root_idx)) = self.pick_dominator_root() else {
            result
                .metadata
                .push("dominators: no entry node available".to_string());
            return result;
        };
        let dom = crate::dominators::Dominators::build(self.graph.inner(), root_idx);
        let root_name = self
            .graph
            .get_node(&root_id)
            .map(|n| n.qualified_name.clone())
            .unwrap_or_else(|| format!("{root_id:?}"));
        result.metadata.push(format!("dominators root={root_name}"));

        let mut nodes = HashSet::new();
        for seed in seeds {
            let Some(seed_idx) = self.graph.resolve(seed) else {
                continue;
            };
            for ancestor_idx in dom.dominators_chain(&seed_idx) {
                if let Some(id) = self.graph.node_id_for(ancestor_idx) {
                    nodes.insert(id.clone());
                }
            }
        }
        fill_nodes(result, nodes, config)
    }

    /// Inverse of [`Self::execute_dominators_of`]: nodes whose dom chain hits any seed.
    pub(super) fn execute_dominates_of(
        &self,
        seeds: &[NodeId],
        config: &QueryConfig,
    ) -> QueryResult {
        let mut result = QueryResult::default();
        let Some((root_id, root_idx)) = self.pick_dominator_root() else {
            result
                .metadata
                .push("dominates: no entry node available".to_string());
            return result;
        };
        let dom = crate::dominators::Dominators::build(self.graph.inner(), root_idx);
        let root_name = self
            .graph
            .get_node(&root_id)
            .map(|n| n.qualified_name.clone())
            .unwrap_or_else(|| format!("{root_id:?}"));
        result.metadata.push(format!("dominates root={root_name}"));

        let seed_idxs: HashSet<petgraph::stable_graph::NodeIndex> = seeds
            .iter()
            .filter_map(|id| self.graph.resolve(id))
            .collect();
        if seed_idxs.is_empty() {
            return result;
        }

        let mut nodes = HashSet::new();
        for idx in self.graph.inner().node_indices() {
            let chain = dom.dominators_chain(&idx);
            for ancestor in chain {
                if seed_idxs.contains(&ancestor) {
                    if let Some(id) = self.graph.node_id_for(idx) {
                        nodes.insert(id.clone());
                    }
                    break;
                }
            }
        }
        fill_nodes(result, nodes, config)
    }

    pub(super) fn pick_dominator_root(
        &self,
    ) -> Option<(NodeId, petgraph::stable_graph::NodeIndex)> {
        for node in self.graph.find_by_name("main") {
            if node.kind == NodeKind::Function
                && node.name == "main"
                && let Some(idx) = self.graph.resolve(&node.id)
            {
                return Some((node.id.clone(), idx));
            }
        }

        let mut best: Option<(usize, NodeId, petgraph::stable_graph::NodeIndex)> = None;
        for func in self.graph.nodes_by_kind(NodeKind::Function) {
            let Some(idx) = self.graph.resolve(&func.id) else {
                continue;
            };
            let fan_in = self.graph.get_edges_to(&func.id).len();
            best = match best {
                None => Some((fan_in, func.id.clone(), idx)),
                Some((best_fan_in, ref best_id, best_idx)) => {
                    if fan_in > best_fan_in || (fan_in == best_fan_in && &func.id < best_id) {
                        Some((fan_in, func.id.clone(), idx))
                    } else {
                        Some((best_fan_in, best_id.clone(), best_idx))
                    }
                }
            };
        }
        best.map(|(_, id, idx)| (id, idx))
    }
}

fn fill_nodes(
    mut result: QueryResult,
    nodes: HashSet<NodeId>,
    config: &QueryConfig,
) -> QueryResult {
    let total = nodes.len();
    let was_truncated = total > config.max_nodes;
    let mut node_list: Vec<NodeId> = nodes.into_iter().collect();
    if was_truncated {
        node_list.truncate(config.max_nodes);
    }
    result.nodes = node_list;
    result.was_truncated = was_truncated;
    result.total_before_truncation = total;
    result
}

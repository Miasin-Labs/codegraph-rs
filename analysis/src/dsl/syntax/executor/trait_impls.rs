use std::collections::HashSet;

use super::{QueryConfig, QueryEngine, QueryResult};
use crate::nodes::NodeId;

impl<'a> QueryEngine<'a> {
    pub(super) fn execute_trait_impls_of(
        &self,
        seeds: &[NodeId],
        config: &QueryConfig,
    ) -> QueryResult {
        let mut result = QueryResult::default();
        let seed_set: HashSet<&NodeId> = seeds.iter().collect();
        let hierarchies = self.graph.trait_hierarchies();
        let mut nodes = HashSet::new();

        for hierarchy in &hierarchies {
            if !seed_set.contains(&hierarchy.trait_id) {
                continue;
            }
            let trait_name = self
                .graph
                .get_node(&hierarchy.trait_id)
                .map(|node| node.qualified_name.clone())
                .unwrap_or_else(|| format!("{:?}", hierarchy.trait_id));
            let impl_names: Vec<String> = hierarchy
                .direct_impls
                .iter()
                .take(8)
                .filter_map(|id| self.graph.get_node(id).map(|node| node.name.clone()))
                .collect();
            result.metadata.push(format!(
                "trait_impls {trait_name} count={} impls=[{}]",
                hierarchy.direct_impls.len(),
                impl_names.join(", ")
            ));
            for id in &hierarchy.direct_impls {
                nodes.insert(id.clone());
            }
        }

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
}

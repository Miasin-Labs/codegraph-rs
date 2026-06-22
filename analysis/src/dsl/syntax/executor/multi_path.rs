use std::collections::HashSet;

use super::super::Expr;
use super::{QueryConfig, QueryEngine, QueryError, QueryResult};
use crate::nodes::NodeId;
use crate::traversal;

impl<'a> QueryEngine<'a> {
    /// Multi-source shortest path - wraps `traversal::find_path_multi_source`.
    pub(super) fn execute_multi_path(
        &self,
        sources: &[Expr],
        to: &Expr,
        max_depth: Option<usize>,
        config: &QueryConfig,
    ) -> Result<QueryResult, QueryError> {
        crate::ensure_sufficient_stack(|| {
            self.execute_multi_path_inner(sources, to, max_depth, config)
        })
    }

    fn execute_multi_path_inner(
        &self,
        sources: &[Expr],
        to: &Expr,
        max_depth: Option<usize>,
        config: &QueryConfig,
    ) -> Result<QueryResult, QueryError> {
        let mut all_sources = HashSet::new();
        for source in sources {
            let result = self.execute_expr(source, config)?;
            for id in result.nodes {
                all_sources.insert(id);
            }
        }
        let to_set = self.execute_expr(to, config)?;
        let depth = max_depth.unwrap_or(32);
        let src_vec: Vec<NodeId> = all_sources.into_iter().collect();
        let mut nodes = HashSet::new();
        let mut edges = Vec::new();
        let mut seen_edges = HashSet::new();
        let mut metadata = Vec::new();

        for target in &to_set.nodes {
            let Some(path) = traversal::find_path_multi_source(self.graph, &src_vec, target, depth)
            else {
                continue;
            };
            metadata.push(format!(
                "multi_path source={:?} to={} len={}",
                self.graph
                    .get_node(&path[0])
                    .map(|n| n.name.clone())
                    .unwrap_or_default(),
                self.graph
                    .get_node(target)
                    .map(|n| n.name.clone())
                    .unwrap_or_default(),
                path.len()
            ));
            for (i, id) in path.iter().enumerate() {
                nodes.insert(id.clone());
                if i + 1 < path.len() {
                    let next = &path[i + 1];
                    let kind_str = self
                        .graph
                        .get_edges_from(id)
                        .into_iter()
                        .find(|(target, _)| *target == next)
                        .map(|(_, edge)| format!("{:?}", edge.kind))
                        .unwrap_or_else(|| "UnknownEdge".to_string());
                    let key = (id.clone(), next.clone(), kind_str);
                    if seen_edges.insert(key.clone()) {
                        edges.push(key);
                    }
                }
            }
            if nodes.len() >= config.max_nodes {
                break;
            }
        }

        let total = nodes.len();
        let was_truncated = total > config.max_nodes;
        let mut node_list: Vec<NodeId> = nodes.into_iter().collect();
        if was_truncated {
            node_list.truncate(config.max_nodes);
        }
        Ok(QueryResult {
            nodes: node_list,
            edges,
            was_truncated,
            total_before_truncation: total,
            cycles_detected: Vec::new(),
            metadata,
        })
    }
}

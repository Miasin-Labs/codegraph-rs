use std::collections::HashSet;

use super::super::{PathMode, PathQuery};
use super::path_search::all_simple_paths_bounded;
use super::{QueryConfig, QueryEngine, QueryError, QueryResult};
use crate::edges::EdgeKind;
use crate::graph::CodeGraph;
use crate::nodes::NodeId;
use crate::traversal;

impl<'a> QueryEngine<'a> {
    /// Execute a `path` / `paths` query.
    pub(super) fn execute_path_query(
        &self,
        pq: &PathQuery,
        config: &QueryConfig,
    ) -> Result<QueryResult, QueryError> {
        crate::ensure_sufficient_stack(|| self.execute_path_query_inner(pq, config))
    }

    fn execute_path_query_inner(
        &self,
        pq: &PathQuery,
        config: &QueryConfig,
    ) -> Result<QueryResult, QueryError> {
        let from_set = self.execute_expr(&pq.from, config)?;
        let to_set = self.execute_expr(&pq.to, config)?;
        let max_depth = pq.max_depth.unwrap_or(32);
        let mut nodes = HashSet::new();
        let mut edges = Vec::new();
        let mut emitted_edges = HashSet::new();

        for src in &from_set.nodes {
            for dst in &to_set.nodes {
                let candidate_paths = match pq.mode {
                    PathMode::Shortest => traversal::find_path(self.graph, src, dst, max_depth)
                        .into_iter()
                        .collect::<Vec<_>>(),
                    PathMode::AllSimple => {
                        all_simple_paths_bounded(self.graph, src, dst, max_depth, config.max_nodes)
                    }
                };

                for path in candidate_paths {
                    if !path_matches_filters(self.graph, &path, pq) {
                        continue;
                    }
                    for (i, node_id) in path.iter().enumerate() {
                        nodes.insert(node_id.clone());
                        if i + 1 < path.len() {
                            let next = &path[i + 1];
                            let kind_str = self
                                .graph
                                .get_edges_from(node_id)
                                .into_iter()
                                .find(|(target, _)| *target == next)
                                .map(|(_, edge)| format!("{:?}", edge.kind))
                                .unwrap_or_else(|| "UnknownEdge".to_string());
                            let key = (node_id.clone(), next.clone(), kind_str);
                            if emitted_edges.insert(key.clone()) {
                                edges.push(key);
                            }
                        }
                    }
                    if nodes.len() >= config.max_nodes {
                        break;
                    }
                }
                if nodes.len() >= config.max_nodes {
                    break;
                }
            }
            if nodes.len() >= config.max_nodes {
                break;
            }
        }

        let total = nodes.len();
        let was_truncated = total > config.max_nodes;
        let node_list: Vec<NodeId> = nodes.into_iter().collect();
        let nodes_out = if was_truncated {
            node_list[..config.max_nodes].to_vec()
        } else {
            node_list
        };

        Ok(QueryResult {
            nodes: nodes_out,
            edges,
            was_truncated,
            total_before_truncation: total,
            cycles_detected: Vec::new(),
            metadata: Vec::new(),
        })
    }
}

fn path_matches_filters(graph: &CodeGraph, path: &[NodeId], pq: &PathQuery) -> bool {
    if let Some(kind) = pq.intermediate_kind
        && path.len() > 2
    {
        for node in &path[1..path.len() - 1] {
            let Some(data) = graph.get_node(node) else {
                return false;
            };
            if data.kind != kind {
                return false;
            }
        }
    }
    if let Some(required) = &pq.via_edge {
        let mut found = false;
        for window in path.windows(2) {
            let from = &window[0];
            let to = &window[1];
            for (target, edge) in graph.get_edges_from(from) {
                if target == to && edge_kind_matches(&edge.kind, required) {
                    found = true;
                    break;
                }
            }
            if found {
                break;
            }
        }
        if !found {
            return false;
        }
    }
    true
}

fn edge_kind_matches(actual: &EdgeKind, expected: &EdgeKind) -> bool {
    use EdgeKind::*;
    matches!(
        (actual, expected),
        (Calls, Calls)
            | (UsesType, UsesType)
            | (References, References)
            | (Contains, Contains)
            | (Implements, Implements)
            | (UnresolvedCall(_), UnresolvedCall(_))
            | (ExternalCall(_, _), ExternalCall(_, _))
            | (Extends, Extends)
            | (Returns, Returns)
            | (TypeOf, TypeOf)
    )
}

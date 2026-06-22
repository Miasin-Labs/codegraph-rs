use std::collections::HashSet;

use super::QueryEngine;
use super::pipe::PipeState;
use crate::nodes::NodeId;

impl<'a> QueryEngine<'a> {
    pub(super) fn apply_hot(&self, n: usize, state: &mut PipeState) {
        if state.working_set.is_empty() {
            let ranked = self.graph.hottest_functions(n);
            state.metadata.push(format!("hot top={n} bare=true"));
            for (id, score) in ranked {
                state.metadata.push(format!(
                    "hot {} score={:.4}",
                    self.graph
                        .get_node(&id)
                        .map(|nd| nd.qualified_name.clone())
                        .unwrap_or_else(|| format!("{id:?}")),
                    score
                ));
                state.working_set.insert(id);
            }
        } else {
            let centrality = self.graph.centrality();
            let mut scored: Vec<(NodeId, f64)> = state
                .working_set
                .iter()
                .map(|id| {
                    (
                        id.clone(),
                        centrality.pagerank.get(id).copied().unwrap_or(0.0),
                    )
                })
                .collect();
            scored.sort_by(|a, b| {
                b.1.partial_cmp(&a.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.0.cmp(&b.0))
            });
            scored.truncate(n);
            state.metadata.push(format!("hot top={n} bare=false"));
            let kept: HashSet<NodeId> = scored.iter().map(|(id, _)| id.clone()).collect();
            for (id, score) in &scored {
                state.metadata.push(format!(
                    "hot {} score={:.4}",
                    self.graph
                        .get_node(id)
                        .map(|nd| nd.qualified_name.clone())
                        .unwrap_or_else(|| format!("{id:?}")),
                    score
                ));
            }
            state.working_set = kept;
        }
    }

    pub(super) fn apply_scc(&self, state: &mut PipeState) {
        let part = self.graph.strongly_connected_components();
        let bare = state.working_set.is_empty();
        let mut keep_components: Vec<usize> = Vec::new();
        if bare {
            for (i, comp) in part.components.iter().enumerate() {
                if comp.len() > 1 {
                    keep_components.push(i);
                }
            }
        } else {
            let mut seen: HashSet<usize> = HashSet::new();
            for id in &state.working_set {
                if let Some(&idx) = part.component_of.get(id)
                    && part.components[idx].len() > 1
                    && seen.insert(idx)
                {
                    keep_components.push(idx);
                }
            }
        }

        let mut new_set = HashSet::new();
        for &i in &keep_components {
            let comp = &part.components[i];
            let names: Vec<String> = comp
                .iter()
                .take(8)
                .filter_map(|id| self.graph.get_node(id).map(|n| n.name.clone()))
                .collect();
            state.metadata.push(format!(
                "SCC[{i}] size={} members=[{}]",
                comp.len(),
                names.join(", ")
            ));
            for id in comp {
                new_set.insert(id.clone());
            }
        }
        state.working_set = new_set;
    }

    pub(super) fn apply_dispatch(&self, state: &mut PipeState) {
        let calls = self.graph.trait_dispatch_calls();
        let mut keep = HashSet::new();
        for d in &calls {
            let in_working = state.working_set.is_empty() || state.working_set.contains(&d.caller);
            if !in_working {
                continue;
            }
            let caller_name = self
                .graph
                .get_node(&d.caller)
                .map(|n| n.qualified_name.clone())
                .unwrap_or_else(|| format!("{:?}", d.caller));
            let callee_name = self
                .graph
                .get_node(&d.callee)
                .map(|n| n.qualified_name.clone())
                .unwrap_or_else(|| format!("{:?}", d.callee));
            let trait_name = self
                .graph
                .get_node(&d.trait_id)
                .map(|n| n.qualified_name.clone())
                .unwrap_or_else(|| format!("{:?}", d.trait_id));
            state.metadata.push(format!(
                "dispatch {caller_name} -> {trait_name}::{callee_name}"
            ));
            keep.insert(d.caller.clone());
        }
        state.working_set = keep;
    }

    pub(super) fn apply_cluster_by_type(&self, state: &mut PipeState) {
        let clusters = self.graph.cluster_by_primary_type();
        let restrict: Option<HashSet<NodeId>> = if state.working_set.is_empty() {
            None
        } else {
            Some(state.working_set.iter().cloned().collect())
        };
        let mut new_set = HashSet::new();
        for (i, c) in clusters.iter().enumerate() {
            let funcs: Vec<NodeId> = match &restrict {
                Some(r) => c
                    .functions
                    .iter()
                    .filter(|f| r.contains(*f))
                    .cloned()
                    .collect(),
                None => c.functions.iter().cloned().collect(),
            };
            if funcs.is_empty() {
                continue;
            }
            let primary_name = self
                .graph
                .get_node(&c.primary_type)
                .map(|n| n.qualified_name.clone())
                .unwrap_or_else(|| format!("{:?}", c.primary_type));
            let member_names: Vec<String> = funcs
                .iter()
                .take(8)
                .filter_map(|id| self.graph.get_node(id).map(|n| n.name.clone()))
                .collect();
            state.metadata.push(format!(
                "cluster[{i}] type={primary_name} size={} members=[{}]",
                funcs.len(),
                member_names.join(", ")
            ));
            for id in funcs {
                new_set.insert(id);
            }
        }
        state.working_set = new_set;
    }

    pub(super) fn apply_affected(&self, depth: usize, since_rev: u64, state: &mut PipeState) {
        let nodes = self.graph.nodes_changed_within_depth(since_rev, depth);
        state.metadata.push(format!(
            "affected depth={depth} since={since_rev} count={}",
            nodes.len()
        ));
        state.working_set = nodes.into_iter().collect();
    }
}

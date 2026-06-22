use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use super::QueryEngine;
use super::pipe::PipeState;
use crate::closure::ClosureDirection;
use crate::label_reachability::{self, PatternAtom};
use crate::nodes::NodeId;

impl<'a> QueryEngine<'a> {
    pub(super) fn apply_co_changes(&self, state: &mut PipeState) {
        let seed_nodes: Vec<NodeId> = state.working_set.iter().cloned().collect();
        let workspace_root = self
            .graph
            .all_node_ids()
            .first()
            .and_then(|id| self.graph.get_node(id))
            .and_then(|n| n.file_path.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));
        let commits = crate::co_change::fetch_git_history(&workspace_root, 500);
        let result = crate::co_change::co_changes_for_nodes(self.graph, &commits, &seed_nodes, 2);
        for pair in &result.pairs {
            let other = if seed_nodes.contains(&pair.node_a) {
                &pair.node_b
            } else {
                &pair.node_a
            };
            state.working_set.insert(other.clone());
            let other_name = self
                .graph
                .get_node(other)
                .map(|n| n.qualified_name.clone())
                .unwrap_or_else(|| format!("{other:?}"));
            state.metadata.push(format!(
                "co_change {} times={} confidence={:.3}",
                other_name, pair.times_changed_together, pair.confidence
            ));
        }
    }

    pub(super) fn apply_communities(&self, state: &mut PipeState) {
        let result = crate::communities::louvain(self.graph, 1.0, 42);
        let restrict: Option<HashSet<NodeId>> = if state.working_set.is_empty() {
            None
        } else {
            Some(state.working_set.iter().cloned().collect())
        };
        let mut by_community: HashMap<u32, Vec<NodeId>> = HashMap::new();
        for (node_id, comm) in &result.assignments {
            let include = match &restrict {
                Some(r) => r.contains(node_id),
                None => true,
            };
            if include {
                by_community.entry(*comm).or_default().push(node_id.clone());
            }
        }

        let mut new_set = HashSet::new();
        let mut sorted_comms: Vec<u32> = by_community.keys().copied().collect();
        sorted_comms.sort();
        for comm in sorted_comms {
            let members = &by_community[&comm];
            let names: Vec<String> = members
                .iter()
                .take(8)
                .filter_map(|id| self.graph.get_node(id).map(|n| n.name.clone()))
                .collect();
            state.metadata.push(format!(
                "community {comm}: [{}]{}",
                names.join(", "),
                if members.len() > 8 {
                    format!(" (+{} more)", members.len() - 8)
                } else {
                    String::new()
                }
            ));
            for id in members {
                new_set.insert(id.clone());
            }
        }
        state.metadata.push(format!(
            "communities total={} modularity={:.4}",
            result.community_count, result.modularity
        ));
        state.working_set = new_set;
    }

    pub(super) fn apply_reachable_via(
        &self,
        pattern: &[PatternAtom],
        direction: ClosureDirection,
        state: &mut PipeState,
    ) {
        let pattern_str = label_reachability::format_pattern(pattern);
        let direction_str = Self::reachable_direction_name(direction);
        if state.working_set.is_empty() {
            state.metadata.push(format!(
                "reachable via \"{pattern_str}\" {direction_str}: empty working set - seed with fn(\"...\") or type(\"...\") first"
            ));
            return;
        }

        let seeds = state.working_set.len();
        let mut reached = HashSet::new();
        for seed in &state.working_set {
            for id in label_reachability::reachable_targets(self.graph, seed, pattern, direction) {
                reached.insert(id);
            }
        }
        state.metadata.push(format!(
            "reachable via \"{pattern_str}\" {direction_str} seeds={seeds} reached={}",
            reached.len()
        ));
        state.working_set = reached;
    }
}

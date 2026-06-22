use std::collections::{HashSet, VecDeque};

use super::pipe::PipeState;
use super::{QueryConfig, QueryEngine};
use crate::edges::EdgeKind;
use crate::nodes::{NodeId, NodeKind};
use crate::traversal::{self, TraversalConfig, TraversalDirection};

impl<'a> QueryEngine<'a> {
    pub(super) fn apply_select_fn(&self, name: &str, state: &mut PipeState) {
        let matches = self.graph.find_by_name(name);
        let total = matches.len();
        state.working_set = matches
            .into_iter()
            .filter(|n| n.kind == NodeKind::Function)
            .map(|n| n.id.clone())
            .collect();
        if state.working_set.is_empty() {
            if total > 0 {
                state.metadata.push(format!(
                    "fn(\"{name}\"): \"{name}\" exists but is not a function ({total} match(es) of other kinds) - try type(\"{name}\")"
                ));
            } else {
                state
                    .metadata
                    .push(format!("fn(\"{name}\"): no symbol named \"{name}\" found"));
            }
        }
    }

    pub(super) fn apply_select_type(&self, name: &str, state: &mut PipeState) {
        let matches = self.graph.find_by_name(name);
        let total = matches.len();
        state.working_set = matches
            .into_iter()
            .filter(|n| matches!(n.kind, NodeKind::Struct | NodeKind::Enum | NodeKind::Trait))
            .map(|n| n.id.clone())
            .collect();
        if state.working_set.is_empty() {
            if total > 0 {
                state.metadata.push(format!(
                    "type(\"{name}\"): \"{name}\" exists but is not a struct/enum/trait ({total} match(es) of other kinds) - try fn(\"{name}\")"
                ));
            } else {
                state.metadata.push(format!(
                    "type(\"{name}\"): no symbol named \"{name}\" found"
                ));
            }
        }
    }

    pub(super) fn apply_callers(&self, state: &mut PipeState) {
        let seed_count = state.working_set.len();
        let non_fn_seeds = state
            .working_set
            .iter()
            .filter_map(|id| self.graph.get_node(id))
            .filter(|n| n.kind != NodeKind::Function)
            .count();
        let mut new_set = HashSet::new();
        for node_id in &state.working_set {
            for (source_id, edge) in self.graph.get_edges_to(node_id) {
                if matches!(edge.kind, EdgeKind::Calls | EdgeKind::UnresolvedCall(_)) {
                    new_set.insert(source_id.clone());
                }
            }
        }
        if new_set.is_empty() && seed_count > 0 {
            if non_fn_seeds == seed_count {
                state.metadata.push(format!(
                    "callers: all {seed_count} seed(s) are types, not functions - types have no call edges. Use fn(\"name\") | callers, or follow uses-type edges instead."
                ));
            } else {
                state.metadata.push(format!(
                    "callers: {seed_count} seed(s) resolved but none has callers (entry point, dead code, or dynamic dispatch)"
                ));
            }
        }
        state.working_set = new_set;
    }

    pub(super) fn apply_callees(&self, state: &mut PipeState) {
        let seed_count = state.working_set.len();
        let non_fn_seeds = state
            .working_set
            .iter()
            .filter_map(|id| self.graph.get_node(id))
            .filter(|n| n.kind != NodeKind::Function)
            .count();
        let mut new_set = HashSet::new();
        for node_id in &state.working_set {
            for (target_id, edge) in self.graph.get_edges_from(node_id) {
                if matches!(edge.kind, EdgeKind::Calls | EdgeKind::UnresolvedCall(_)) {
                    new_set.insert(target_id.clone());
                }
            }
        }
        if new_set.is_empty() && seed_count > 0 {
            if non_fn_seeds == seed_count {
                state.metadata.push(format!(
                    "callees: all {seed_count} seed(s) are types, not functions - types have no call edges. Use fn(\"name\") | callees."
                ));
            } else {
                state.metadata.push(format!(
                    "callees: {seed_count} seed(s) resolved but none calls anything (leaf function or unresolved external calls)"
                ));
            }
        }
        state.working_set = new_set;
    }

    pub(super) fn apply_depth(&self, depth: usize, config: &QueryConfig, state: &mut PipeState) {
        let mut expanded = HashSet::new();
        for node_id in &state.working_set {
            let result = traversal::traverse(
                self.graph,
                node_id,
                &TraversalConfig {
                    max_depth: depth,
                    max_nodes: config.max_nodes,
                    direction: TraversalDirection::Outgoing,
                    parallel: false,
                },
            );
            for id in result.nodes {
                expanded.insert(id);
            }
            state.cycles_detected.extend(result.cycles_detected_at);
        }
        state.working_set = expanded;
    }

    pub(super) fn apply_filter(&self, kind: NodeKind, state: &mut PipeState) {
        state.working_set.retain(|id| {
            self.graph
                .get_node(id)
                .map(|n| n.kind == kind)
                .unwrap_or(false)
        });
    }

    pub(super) fn apply_since(&self, rev: u64, state: &mut PipeState) {
        state.working_set.retain(|id| {
            self.graph
                .get_node(id)
                .map(|n| n.last_modified_revision >= rev)
                .unwrap_or(false)
        });
    }

    pub(super) fn apply_preconditions(&self, config: &QueryConfig, state: &mut PipeState) {
        self.apply_call_reachability(config, state, true);
    }

    pub(super) fn apply_taint(&self, config: &QueryConfig, state: &mut PipeState) {
        self.apply_call_reachability(config, state, false);
    }

    fn apply_call_reachability(&self, config: &QueryConfig, state: &mut PipeState, incoming: bool) {
        let mut reached = HashSet::new();
        let mut visited = HashSet::new();
        let mut queue: VecDeque<NodeId> = state.working_set.iter().cloned().collect();

        while let Some(current) = queue.pop_front() {
            if visited.contains(&current) {
                state.cycles_detected.push(current);
                continue;
            }
            visited.insert(current.clone());
            reached.insert(current.clone());

            if incoming {
                for (source_id, edge) in self.graph.get_edges_to(&current) {
                    if matches!(edge.kind, EdgeKind::Calls | EdgeKind::UnresolvedCall(_)) {
                        if visited.contains(source_id) {
                            state.cycles_detected.push(source_id.clone());
                        } else {
                            queue.push_back(source_id.clone());
                        }
                    }
                }
            } else {
                for (target_id, edge) in self.graph.get_edges_from(&current) {
                    if matches!(edge.kind, EdgeKind::Calls | EdgeKind::UnresolvedCall(_)) {
                        if visited.contains(target_id) {
                            state.cycles_detected.push(target_id.clone());
                        } else {
                            queue.push_back(target_id.clone());
                        }
                    }
                }
            }

            if reached.len() >= config.max_nodes {
                break;
            }
        }

        state.working_set = reached;
    }
}

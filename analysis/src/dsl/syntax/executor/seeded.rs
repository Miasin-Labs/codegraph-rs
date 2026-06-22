use std::collections::HashSet;

use super::super::DslOp;
use super::pipe::PipeState;
use super::{QueryConfig, QueryEngine, QueryError, QueryResult};
use crate::nodes::{NodeId, NodeKind};

impl<'a> QueryEngine<'a> {
    /// Execute pipe ops against a pre-seeded working set.
    pub(super) fn execute_pipe_from(
        &self,
        seed: &[NodeId],
        ops: &[DslOp],
        config: &QueryConfig,
    ) -> Result<QueryResult, QueryError> {
        let mut state = PipeState::with_seed(seed);
        for op in ops {
            self.apply_seeded_pipe_op(op, &mut state);
        }
        Ok(state.finish(self.graph, config))
    }

    fn apply_seeded_pipe_op(&self, op: &DslOp, state: &mut PipeState) {
        match op {
            DslOp::SelectFn(name) => {
                state.working_set = self
                    .graph
                    .find_by_name(name)
                    .into_iter()
                    .filter(|n| n.kind == NodeKind::Function)
                    .map(|n| n.id.clone())
                    .collect();
            }
            DslOp::SelectType(name) => {
                state.working_set = self
                    .graph
                    .find_by_name(name)
                    .into_iter()
                    .filter(|n| {
                        matches!(n.kind, NodeKind::Struct | NodeKind::Enum | NodeKind::Trait)
                    })
                    .map(|n| n.id.clone())
                    .collect();
            }
            DslOp::Filter(kind) => self.apply_filter(*kind, state),
            DslOp::Since(rev) => self.apply_since(*rev, state),
            DslOp::Depth(depth) => self.apply_seeded_depth(*depth, state),
            DslOp::Untested => self.apply_untested(state),
            DslOp::PossibleTypes => self.apply_possible_types(state),
            DslOp::CoChanges => self.apply_co_changes(state),
            DslOp::Communities => self.apply_communities(state),
            _ => {}
        }
    }

    fn apply_seeded_depth(&self, max_depth: usize, state: &mut PipeState) {
        let current: Vec<NodeId> = state.working_set.iter().cloned().collect();
        for _ in 0..max_depth {
            let mut next_layer = HashSet::new();
            for id in &current {
                for (target_id, _) in self.graph.get_edges_from(id) {
                    next_layer.insert(target_id.clone());
                }
            }
            state.working_set.extend(next_layer);
        }
    }
}

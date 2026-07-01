use super::super::DslOp;
use super::pipe::PipeState;
use super::{QueryConfig, QueryEngine, QueryError, QueryResult};
use crate::nodes::NodeId;

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
            self.apply_pipe_op(op, config, &mut state);
        }
        Ok(state.finish(self.graph, config))
    }
}

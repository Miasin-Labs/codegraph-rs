use std::collections::HashSet;

use super::super::{DslOp, parse_query};
use super::{QueryConfig, QueryEngine, QueryError, QueryResult};
use crate::closure::ClosureDirection;
use crate::graph::CodeGraph;
use crate::nodes::NodeId;

#[derive(Default)]
pub(super) struct PipeState {
    pub(super) working_set: HashSet<NodeId>,
    pub(super) cycles_detected: Vec<NodeId>,
    pub(super) metadata: Vec<String>,
}

impl PipeState {
    pub(super) fn with_seed(seed: &[NodeId]) -> Self {
        Self {
            working_set: seed.iter().cloned().collect(),
            cycles_detected: Vec::new(),
            metadata: Vec::new(),
        }
    }

    pub(super) fn finish(self, graph: &CodeGraph, config: &QueryConfig) -> QueryResult {
        let node_list: Vec<NodeId> = self.working_set.into_iter().collect();
        let node_set: HashSet<&NodeId> = node_list.iter().collect();
        let mut edges = Vec::new();
        for node_id in &node_list {
            for (target, edge_data) in graph.get_edges_from(node_id) {
                if node_set.contains(target) {
                    edges.push((
                        node_id.clone(),
                        target.clone(),
                        format!("{:?}", edge_data.kind),
                    ));
                }
            }
        }

        let total = node_list.len();
        let was_truncated = total > config.max_nodes;
        let nodes = if was_truncated {
            node_list[..config.max_nodes].to_vec()
        } else {
            node_list
        };

        QueryResult {
            nodes,
            edges,
            was_truncated,
            total_before_truncation: total,
            cycles_detected: self.cycles_detected,
            metadata: self.metadata,
        }
    }
}

impl<'a> QueryEngine<'a> {
    /// Execute a parsed query against the graph.
    pub fn execute(&self, ops: &[DslOp], config: &QueryConfig) -> Result<QueryResult, QueryError> {
        let mut state = PipeState::default();
        for op in ops {
            self.apply_pipe_op(op, config, &mut state);
        }
        Ok(state.finish(self.graph, config))
    }

    pub(super) fn apply_pipe_op(&self, op: &DslOp, config: &QueryConfig, state: &mut PipeState) {
        match op {
            DslOp::SelectFn(name) => self.apply_select_fn(name, state),
            DslOp::SelectType(name) => self.apply_select_type(name, state),
            DslOp::Callers => self.apply_callers(state),
            DslOp::Callees => self.apply_callees(state),
            DslOp::Depth(depth) => self.apply_depth(*depth, config, state),
            DslOp::Filter(kind) => self.apply_filter(*kind, state),
            DslOp::Show(_) => {}
            DslOp::Taint(_) => self.apply_taint(config, state),
            DslOp::Preconditions => self.apply_preconditions(config, state),
            DslOp::Since(rev) => self.apply_since(*rev, state),
            DslOp::Hot(n) => self.apply_hot(*n, state),
            DslOp::Scc => self.apply_scc(state),
            DslOp::Dispatch => self.apply_dispatch(state),
            DslOp::ClusterByType => self.apply_cluster_by_type(state),
            DslOp::Affected { depth, since_rev } => self.apply_affected(*depth, *since_rev, state),
            DslOp::Untested => self.apply_untested(state),
            DslOp::PossibleTypes => self.apply_possible_types(state),
            DslOp::CoChanges => self.apply_co_changes(state),
            DslOp::Communities => self.apply_communities(state),
            DslOp::Complexity => self.apply_complexity(state),
            DslOp::Cfg => self.apply_cfg(state),
            DslOp::Dataflow => self.apply_dataflow(state),
            DslOp::ReachableVia { pattern, direction } => {
                self.apply_reachable_via(pattern, *direction, state);
            }
        }
    }

    pub(super) fn reachable_direction_name(direction: ClosureDirection) -> &'static str {
        match direction {
            ClosureDirection::Outgoing => "outgoing",
            ClosureDirection::Incoming => "incoming",
        }
    }
}

/// Convenience function: parse and execute a query string.
///
/// Phase 3: runs through the [`crate::dsl::plan`] optimiser before execution.
pub fn run_query(
    query: &str,
    graph: &CodeGraph,
    config: &QueryConfig,
) -> Result<QueryResult, QueryError> {
    let ops = parse_query(query)?;
    let plan = crate::dsl::plan::optimise_pipe(ops);
    let engine = QueryEngine::new(graph);
    let ops = plan.ops().expect("optimise_pipe yields Plan::Pipe");
    engine.execute(ops, config)
}

use super::{QueryConfig, QueryError, QueryResult};
use crate::graph::CodeGraph;
use crate::nodes::NodeId;

/// Internal: run a query through the aggregation executor and project its result.
pub(super) fn run_aggregate_unified(
    query: &str,
    graph: &CodeGraph,
    config: &QueryConfig,
) -> Result<QueryResult, QueryError> {
    use crate::dsl::aggregate::{AggregateResult, run_aggregate};
    let result = run_aggregate(query, graph, config)?;
    let (nodes, edges_meta, metadata) = match result {
        AggregateResult::Nodes(nodes) => (nodes, Vec::new(), Vec::new()),
        AggregateResult::Edges(edges) => {
            let nodes: Vec<NodeId> = edges
                .iter()
                .flat_map(|edge| [edge.from.clone(), edge.to.clone()])
                .collect();
            let edges_meta: Vec<(NodeId, NodeId, String)> = edges
                .iter()
                .map(|edge| {
                    (
                        edge.from.clone(),
                        edge.to.clone(),
                        format!("{:?}", edge.kind),
                    )
                })
                .collect();
            let metadata: Vec<String> = edges
                .iter()
                .map(|edge| format!("edge {:?} {} -> {}", edge.kind, "from", "to"))
                .take(10)
                .collect();
            (nodes, edges_meta, metadata)
        }
        AggregateResult::Scalar(n) => (Vec::new(), Vec::new(), vec![format!("scalar = {n}")]),
        AggregateResult::Bool(b) => (Vec::new(), Vec::new(), vec![format!("bool = {b}")]),
        AggregateResult::Groups(groups) => {
            let nodes: Vec<NodeId> = groups.values().flatten().cloned().collect();
            let metadata: Vec<String> = groups
                .iter()
                .map(|(key, values)| format!("group `{key}` size={}", values.len()))
                .collect();
            (nodes, Vec::new(), metadata)
        }
    };
    Ok(QueryResult {
        nodes,
        edges: edges_meta,
        was_truncated: false,
        total_before_truncation: 0,
        cycles_detected: Vec::new(),
        metadata,
    })
}

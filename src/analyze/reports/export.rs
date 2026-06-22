use super::*;

// =============================================================================
// analyze export
// =============================================================================

/// Node cap for `--symbol` subgraph exports — keeps the DOT renderable.
const EXPORT_SUBGRAPH_MAX_NODES: usize = 2000;

/// Result of [`export_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportReport {
    /// Output format (only `dot` today).
    pub format: String,
    /// `graph` (everything) or `subgraph` (neighborhood of `seed`).
    pub scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<SymbolRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<usize>,
    pub node_count: usize,
    /// The Graphviz DOT document (pipe to `dot -Tsvg`).
    pub dot: String,
}

/// Graphviz DOT export of the bridged graph (engine entry points:
/// `analysis::to_dot`, `analysis::to_dot_subgraph`). With a seed, exports the
/// `depth`-hop neighborhood (both directions). Returns `None` if a seed was
/// given but is not in the graph.
pub fn export_report(
    graph: &AnalysisGraph,
    seed: Option<&ANodeId>,
    depth: usize,
) -> Option<ExportReport> {
    match seed {
        Some(id) => {
            let seed_node = graph.get_node(id)?;
            let config = TraversalConfig {
                max_depth: depth,
                max_nodes: EXPORT_SUBGRAPH_MAX_NODES,
                direction: TraversalDirection::Both,
                parallel: false,
            };
            let walk = traverse(graph, id, &config);
            let dot = analysis::to_dot_subgraph(graph, &walk.nodes);
            Some(ExportReport {
                format: "dot".to_string(),
                scope: "subgraph".to_string(),
                seed: Some(symbol_ref(seed_node)),
                depth: Some(depth),
                node_count: walk.nodes.len(),
                dot,
            })
        }
        None => Some(ExportReport {
            format: "dot".to_string(),
            scope: "graph".to_string(),
            seed: None,
            depth: None,
            node_count: graph.node_count(),
            dot: analysis::to_dot(graph),
        }),
    }
}

use super::*;

// =============================================================================
// analyze dominators
// =============================================================================

/// Immediate-dominator record for one node reachable from the entry.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DominatorEntry {
    pub symbol: SymbolRef,
    /// The node every path from the entry must pass through last before
    /// reaching this one. `None` only for the entry itself (not listed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub immediate_dominator: Option<SymbolRef>,
    /// Length of the dominator chain back to the entry.
    pub dominator_depth: usize,
}

/// Result of [`dominators_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DominatorsReport {
    pub entry: SymbolRef,
    /// Nodes analyzed (reachable from the entry, capped by `--top`).
    pub analyzed: usize,
    /// True when the reachability walk hit the cap.
    pub truncated: bool,
    pub nodes: Vec<DominatorEntry>,
}

/// Dominator analysis rooted at `entry`: for each node reachable from the
/// entry (BFS order, capped at `limit`), report its immediate dominator and
/// chain depth. Returns `None` if `entry` is not in the graph.
///
/// Computes the dominator tree once (`analysis::dominator_tree`) and reads
/// every chain off it, instead of the previous per-node
/// `dominator_chain` recomputation.
pub fn dominators_report(
    graph: &AnalysisGraph,
    entry: &ANodeId,
    limit: usize,
) -> Option<DominatorsReport> {
    let entry_node = graph.get_node(entry)?;
    let tree = analysis::dominator_tree(graph, entry)?;

    let config = TraversalConfig {
        max_depth: DOMINATOR_TRAVERSAL_DEPTH,
        max_nodes: limit.saturating_add(1),
        direction: TraversalDirection::Outgoing,
        parallel: false,
    };
    let walk = traverse(graph, entry, &config);

    let mut nodes: Vec<DominatorEntry> = Vec::new();
    for id in walk.nodes.iter().filter(|id| *id != entry) {
        let Some(node) = graph.get_node(id) else {
            continue;
        };
        let chain = tree.chain_report(id);
        if chain.dominators.is_empty() {
            // Unreachable under dominance (shouldn't happen for BFS-reached
            // nodes, but never report a node without a chain to the entry).
            continue;
        }
        let immediate_dominator = chain
            .dominators
            .first()
            .and_then(|d| graph.get_node(d))
            .map(symbol_ref);
        nodes.push(DominatorEntry {
            symbol: symbol_ref(node),
            immediate_dominator,
            dominator_depth: chain.dominators.len(),
        });
    }

    Some(DominatorsReport {
        entry: symbol_ref(entry_node),
        analyzed: nodes.len(),
        truncated: walk.was_truncated,
        nodes,
    })
}

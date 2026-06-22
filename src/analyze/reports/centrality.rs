use super::*;

// =============================================================================
// analyze centrality / analyze critical
// =============================================================================

/// PageRank damping factor (the standard 0.85).
const CENTRALITY_DAMPING: f32 = 0.85;

/// One node ranked by PageRank score.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RankedSymbol {
    pub symbol: SymbolRef,
    pub score: f64,
}

/// Result of [`centrality_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CentralityReport {
    pub damping_factor: f64,
    /// Nodes ranked (the whole graph; placeholders excluded from the listing).
    pub analyzed: usize,
    pub nodes: Vec<RankedSymbol>,
}

/// PageRank centrality over the bridged graph (engine entry point:
/// `analysis::centrality`) — the most depended-upon symbols first.
pub fn centrality_report(graph: &AnalysisGraph, top: usize) -> CentralityReport {
    let analyzed = graph.node_count();
    let mut nodes: Vec<RankedSymbol> = analysis::centrality(graph, analyzed, CENTRALITY_DAMPING)
        .into_iter()
        .filter_map(|r| {
            let node = graph.get_node(&r.id)?;
            if is_placeholder(node) {
                return None;
            }
            Some(RankedSymbol {
                symbol: symbol_ref(node),
                score: r.score,
            })
        })
        .collect();
    nodes.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| symbol_sort_key(&a.symbol).cmp(&symbol_sort_key(&b.symbol)))
    });
    nodes.truncate(top);

    CentralityReport {
        damping_factor: CENTRALITY_DAMPING as f64,
        analyzed,
        nodes,
    }
}

/// A fragile coupling edge whose removal disconnects the graph.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeEdgeSummary {
    pub from: SymbolRef,
    pub to: SymbolRef,
}

/// Result of [`critical_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CriticalReport {
    /// Articulation nodes found (before truncation).
    pub articulation_count: usize,
    /// Bridge edges found (before truncation).
    pub bridge_count: usize,
    pub truncated: bool,
    pub nodes: Vec<SymbolRef>,
    pub bridges: Vec<BridgeEdgeSummary>,
    pub note: String,
}

/// Articulation nodes + bridge edges (engine entry points:
/// `analysis::critical_nodes`, `analysis::bridge_edges`) — the single points
/// of failure in the dependency structure.
pub fn critical_report(graph: &AnalysisGraph, top: usize) -> CriticalReport {
    let mut nodes: Vec<SymbolRef> = analysis::critical_nodes(graph)
        .into_iter()
        .filter_map(|id| {
            let node = graph.get_node(&id)?;
            if is_placeholder(node) {
                return None;
            }
            Some(symbol_ref(node))
        })
        .collect();
    nodes.sort();
    let articulation_count = nodes.len();

    let mut bridges: Vec<BridgeEdgeSummary> = analysis::bridge_edges(graph)
        .into_iter()
        .filter_map(|e| {
            let from = graph.get_node(&e.from)?;
            let to = graph.get_node(&e.to)?;
            if is_placeholder(from) || is_placeholder(to) {
                return None;
            }
            Some(BridgeEdgeSummary {
                from: symbol_ref(from),
                to: symbol_ref(to),
            })
        })
        .collect();
    bridges.sort_by(|a, b| {
        (symbol_sort_key(&a.from), symbol_sort_key(&a.to))
            .cmp(&(symbol_sort_key(&b.from), symbol_sort_key(&b.to)))
    });
    let bridge_count = bridges.len();

    let truncated = articulation_count > top || bridge_count > top;
    nodes.truncate(top);
    bridges.truncate(top);

    CriticalReport {
        articulation_count,
        bridge_count,
        truncated,
        nodes,
        bridges,
        note: "Computed over the graph treated as undirected: removing an articulation node \
               (or bridge edge) disconnects callers from callees regardless of call direction."
            .to_string(),
    }
}

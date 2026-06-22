use super::*;

/// Render a [`DslQueryError`] without the redundant outer `parse error:`
/// wrapper — the inner parse error already carries position + offending
/// token, which is what a CLI user needs to fix the query.
fn friendly_query_error(err: DslQueryError) -> String {
    match err {
        DslQueryError::Parse(parse) => parse.to_string(),
        other => other.to_string(),
    }
}
/// Why-provenance via the engine's `trace_query`, projected onto
/// [`WhyEntry`] rows for the final result nodes. Returns `None` when the
/// query shape cannot be traced (the aggregation grammar has no pipe to
/// replay) — callers surface that as "unavailable", never as an error.
fn build_why(graph: &AnalysisGraph, query: &str, config: &DslQueryConfig) -> Option<Vec<WhyEntry>> {
    let trace = trace_query(query, graph, config).ok()?;
    let mut entries: Vec<WhyEntry> = trace
        .result_nodes
        .iter()
        .filter_map(|id| {
            let node = graph.get_node(id)?;
            let prov = trace.entries.get(id)?;
            let steps = prov
                .steps
                .iter()
                .map(|step| WhyStep {
                    op: step.op_name.clone(),
                    predecessors: step
                        .predecessors
                        .iter()
                        .filter_map(|p| graph.get_node(p))
                        .map(|n| n.qualified_name.clone())
                        .collect(),
                    stage: step.depth,
                })
                .collect();
            Some(WhyEntry {
                symbol: symbol_ref(node),
                steps,
            })
        })
        .collect();
    entries.sort_by(|a, b| symbol_sort_key(&a.symbol).cmp(&symbol_sort_key(&b.symbol)));
    Some(entries)
}
/// Run a DSL query over the bridged graph through the engine's unified
/// entry point (`run_query_expr`: pipe chains, set algebra, path patterns,
/// entrypoint/dominator selectors, and aggregations), including the plan
/// optimiser. Parse errors come back as the parser's own message (position
/// + offending token) so the CLI can show them verbatim.
///
/// Equivalent to [`query_report_with_sources`] without source-level
/// enrichment (no workspace root to read files under).
pub fn query_report(
    graph: &AnalysisGraph,
    query: &str,
    max_nodes: usize,
    include_why: bool,
) -> Result<QueryReport, String> {
    query_report_with_sources(graph, query, max_nodes, include_why, None)
}

/// [`query_report`] plus source-level enrichment: when `source_root` is
/// given and the query contains the `preconditions` operator, the report
/// gains a [`PreconditionsSection`] with the actual guarding conditions
/// extracted from the on-disk sources.
pub fn query_report_with_sources(
    graph: &AnalysisGraph,
    query: &str,
    max_nodes: usize,
    include_why: bool,
    source_root: Option<&Path>,
) -> Result<QueryReport, String> {
    let config = DslQueryConfig {
        max_nodes,
        ..Default::default()
    };
    let result = run_query_expr(query, graph, &config).map_err(friendly_query_error)?;

    let mut nodes: Vec<SymbolRef> = result
        .nodes
        .iter()
        .filter_map(|id| graph.get_node(id))
        .map(symbol_ref)
        .collect();
    nodes.sort_by(|a, b| symbol_sort_key(a).cmp(&symbol_sort_key(b)));

    let edges: Vec<QueryEdge> = result
        .edges
        .iter()
        .filter_map(|(from, to, kind)| {
            let from = graph.get_node(from)?;
            let to = graph.get_node(to)?;
            Some(QueryEdge {
                from: from.qualified_name.clone(),
                to: to.qualified_name.clone(),
                kind: kind.clone(),
            })
        })
        .collect();

    let mut cycles: Vec<SymbolRef> = result
        .cycles_detected
        .iter()
        .filter_map(|id| graph.get_node(id))
        .map(symbol_ref)
        .collect();
    cycles.sort();
    cycles.dedup();

    let why = if include_why {
        build_why(graph, query, &config)
    } else {
        None
    };

    let preconditions = match source_root {
        Some(root) if query_requests_preconditions(query) => {
            Some(build_preconditions_section(graph, root, &result.nodes))
        }
        _ => None,
    };

    Ok(QueryReport {
        query: query.to_string(),
        node_count: nodes.len(),
        total_before_truncation: result.total_before_truncation,
        truncated: result.was_truncated,
        nodes,
        edges,
        metadata: result.metadata,
        cycles_detected: cycles,
        why,
        preconditions,
    })
}

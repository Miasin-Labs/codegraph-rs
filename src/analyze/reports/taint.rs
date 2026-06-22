use super::*;

// =============================================================================
// analyze taint
// =============================================================================

/// One source-to-sink path.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaintPathSummary {
    pub nodes: Vec<SymbolRef>,
    /// Edge kind of each hop (`nodes.len() - 1` entries).
    pub edge_kinds: Vec<String>,
}

/// Result of [`taint_report`] (and of
/// [`crate::analyze_ir::value_taint_report`], which upgrades the oracle).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaintReport {
    pub source: SymbolRef,
    pub sink: SymbolRef,
    pub max_intermediate_nodes: usize,
    /// `"call-graph"` from [`taint_report`]; `"value-level"` when
    /// `analyze taint --value-level` runs over dataflow IR — see `note`.
    pub granularity: String,
    /// Total simple paths found (before capping `paths`).
    pub path_count: usize,
    pub truncated: bool,
    pub paths: Vec<TaintPathSummary>,
    /// IR-lowering coverage, present on value-level runs only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ir_coverage: Option<crate::analyze_ir::IrCoverage>,
    pub note: String,
}

pub(crate) fn edge_label_between(graph: &AnalysisGraph, from: &ANodeId, to: &ANodeId) -> String {
    let mut labels: Vec<&'static str> = graph
        .get_edges_from(from)
        .into_iter()
        .filter(|(target, _)| *target == to)
        .map(|(_, edge)| edge_kind_label(&edge.kind))
        .collect();
    if labels.contains(&"calls") {
        return "calls".to_string();
    }
    labels.sort();
    labels.first().copied().unwrap_or("unknown").to_string()
}

/// All simple paths from `source` to `sink` (the analysis crate's
/// `taint_paths` primitive), each hop annotated with its edge kind. This is
/// graph reachability between the two symbols — the engine's sanitizer-aware
/// value-level taint needs dataflow IR the bridge does not provide, and that
/// limitation is stated in the report instead of being papered over.
/// Returns `None` if either endpoint is not in the graph.
pub fn taint_report(
    graph: &AnalysisGraph,
    source: &ANodeId,
    sink: &ANodeId,
    max_intermediate_nodes: usize,
    max_paths: usize,
) -> Option<TaintReport> {
    let source_node = graph.get_node(source)?;
    let sink_node = graph.get_node(sink)?;

    let mut raw = analysis::taint_paths(graph, source, sink, max_intermediate_nodes);
    raw.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    let path_count = raw.len();
    let truncated = path_count > max_paths;
    raw.truncate(max_paths);

    let paths: Vec<TaintPathSummary> = raw
        .into_iter()
        .map(|path| {
            let edge_kinds = path
                .windows(2)
                .map(|pair| edge_label_between(graph, &pair[0], &pair[1]))
                .collect();
            let nodes = path
                .iter()
                .filter_map(|id| graph.get_node(id))
                .map(symbol_ref)
                .collect();
            TaintPathSummary { nodes, edge_kinds }
        })
        .collect();

    Some(TaintReport {
        source: symbol_ref(source_node),
        sink: symbol_ref(sink_node),
        max_intermediate_nodes,
        granularity: "call-graph".to_string(),
        path_count,
        truncated,
        paths,
        ir_coverage: None,
        note: call_graph_granularity_note("Source-to-sink paths"),
    })
}

/// Result of [`source_taint_report`] — `analyze taint --source`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceTaintReport {
    pub source: String,
    pub sink: String,
    /// Rendered-flow cap (the engine appends a "… and N more flow(s)"
    /// trailer when more flows exist).
    pub max_paths: usize,
    /// The engine's source-annotated taint-flow report: each flow's full
    /// path rendered hop by hop with sanitizer status.
    pub report: String,
    pub coverage: SourceReportCoverage,
    pub note: String,
}

/// Source-annotated source→sink taint flow via the engine's CPG report
/// façade (engine entry point: `analysis_tools::taint_flow`). The façade
/// resolves both symbols by name, runs the engine's sanitizer-aware
/// value-level taint over the points-to oracle, and caps the rendered
/// flows at `max_paths` — flows beyond the cap are summarized, never
/// silently dropped.
pub fn source_taint_report(
    graph: &AnalysisGraph,
    workspace_root: &Path,
    source: &str,
    sink: &str,
    max_paths: usize,
) -> SourceTaintReport {
    let report = analysis_tools::taint_flow(
        graph,
        &[source.to_string()],
        &[sink.to_string()],
        &[],
        max_paths,
    );
    let (functions_total, functions_missing_byte_range) = function_byte_presence(graph);
    let coverage = SourceReportCoverage {
        functions_total,
        functions_missing_byte_range,
    };
    let lead = format!("Rendered flows cap at {max_paths} paths.");
    let note = source_report_note(workspace_root, &lead, &coverage);
    SourceTaintReport {
        source: source.to_string(),
        sink: sink.to_string(),
        max_paths,
        report,
        coverage,
        note,
    }
}

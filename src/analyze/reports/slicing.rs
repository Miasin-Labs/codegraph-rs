use super::*;

// =============================================================================
// analyze slice
// =============================================================================

/// Direction of a program slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceDirection {
    /// What the seed's values can influence (walks callees).
    Forward,
    /// What can affect the values reaching the seed (walks callers).
    Backward,
}

impl SliceDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            SliceDirection::Forward => "forward",
            SliceDirection::Backward => "backward",
        }
    }
}

/// Result of [`slice_report`] (and of
/// [`crate::analyze_ir::value_slice_report`], which upgrades the oracle).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SliceReport {
    pub seed: SymbolRef,
    pub direction: String,
    pub max_depth: usize,
    /// `"call-graph"` from [`slice_report`]; `"value-level"` when
    /// `analyze slice --value-level` runs over dataflow IR — see `note`.
    pub granularity: String,
    /// Slice size excluding the seed.
    pub size: usize,
    pub nodes: Vec<SymbolRef>,
    /// IR-lowering coverage, present on value-level runs only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ir_coverage: Option<crate::analyze_ir::IrCoverage>,
    pub note: String,
}

/// Capability note shared by slice and taint reports.
pub(crate) fn call_graph_granularity_note(what: &str) -> String {
    format!(
        "{what} computed at call-graph granularity (hops follow resolved and \
         unresolved call edges). Re-run with --value-level for def-use \
         precision: it lowers per-function dataflow IR by re-parsing the \
         working tree (needs byte offsets in the index — re-index pre-v5 \
         projects to enable)."
    )
}

/// Forward/backward program slice from `seed` using the analysis crate's
/// slicing algorithms over the [`CallGraphOracle`]. Returns `None` if `seed`
/// is not in the graph.
pub fn slice_report(
    graph: &AnalysisGraph,
    seed: &ANodeId,
    direction: SliceDirection,
    max_depth: usize,
) -> Option<SliceReport> {
    let seed_node = graph.get_node(seed)?;
    let oracle = CallGraphOracle::build(graph);
    let set = match direction {
        SliceDirection::Forward => forward_slice(graph, &oracle, seed, max_depth),
        SliceDirection::Backward => backward_slice(graph, &oracle, seed, max_depth),
    };

    let mut nodes: Vec<SymbolRef> = set
        .iter()
        .filter(|id| *id != seed)
        .filter_map(|id| graph.get_node(id))
        .map(symbol_ref)
        .collect();
    nodes.sort_by(|a, b| (&a.file, a.line, &a.name).cmp(&(&b.file, b.line, &b.name)));

    Some(SliceReport {
        seed: symbol_ref(seed_node),
        direction: direction.as_str().to_string(),
        max_depth,
        granularity: "call-graph".to_string(),
        size: nodes.len(),
        nodes,
        ir_coverage: None,
        note: call_graph_granularity_note("Slice"),
    })
}

/// Result of [`source_slice_report`] — `analyze slice --source`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSliceReport {
    /// The symbol as given (the engine façade resolves it by name,
    /// qualified-suffix aware).
    pub symbol: String,
    pub direction: String,
    /// Entry cap applied to both annotated lists.
    pub max_entries: usize,
    /// The engine's compact source-annotated slice report (one
    /// `name (file:line)` entry per line, capped at `max_entries`).
    pub report: String,
    /// The engine's one-hop incoming data-dependency report, same
    /// annotation and cap.
    pub data_dependencies: String,
    pub coverage: SourceReportCoverage,
    pub note: String,
}

/// Source-annotated program slice via the engine's CPG report façade
/// (engine entry points: `analysis_tools::program_slice` +
/// `analysis_tools::data_dependencies`). The façade resolves `symbol` by
/// name, builds the interprocedural IR map + points-to oracle itself, and
/// renders every slice node as `name (file:line)` — value-level fidelity
/// when the index carries byte offsets (schema v5), honest degradation
/// notes otherwise. The slice depth is the engine's fixed default
/// (currently 6 hops); `max_entries` caps the rendered lists.
pub fn source_slice_report(
    graph: &AnalysisGraph,
    workspace_root: &Path,
    symbol: &str,
    direction: SliceDirection,
    max_entries: usize,
) -> SourceSliceReport {
    let backward = matches!(direction, SliceDirection::Backward);
    let report = analysis_tools::program_slice(graph, symbol, backward, max_entries);
    let data_dependencies = analysis_tools::data_dependencies(graph, symbol, max_entries);
    let (functions_total, functions_missing_byte_range) = function_byte_presence(graph);
    let coverage = SourceReportCoverage {
        functions_total,
        functions_missing_byte_range,
    };
    let lead = format!(
        "Slice depth is the engine's fixed default (6 hops); both lists cap at {max_entries} \
         entries."
    );
    let note = source_report_note(workspace_root, &lead, &coverage);
    SourceSliceReport {
        symbol: symbol.to_string(),
        direction: direction.as_str().to_string(),
        max_entries,
        report,
        data_dependencies,
        coverage,
        note,
    }
}

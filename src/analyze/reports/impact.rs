use super::{ANodeId, AnalysisGraph, Serialize, SymbolRef, generate_cascade, symbol_ref};

// =============================================================================
// analyze impact (signature-edit cascade)
// =============================================================================

/// A call site that needs updating after a signature edit.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImpactCallSite {
    pub caller: String,
    pub file: String,
    pub line: u32,
}

/// One per-file cascade task.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CascadeTaskSummary {
    pub file: String,
    pub instruction: String,
    pub call_sites: Vec<ImpactCallSite>,
}

/// Result of [`impact_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImpactReport {
    pub target: SymbolRef,
    pub new_signature: String,
    pub task_count: usize,
    pub call_site_count: usize,
    pub tasks: Vec<CascadeTaskSummary>,
}

/// The analysis engine's signature-edit cascade for `target`: every direct
/// call site that must be updated, grouped into one task per file. This is
/// per-call-site precise and distinct from the BFS impact radius of
/// `codegraph impact`. Returns `None` if `target` is not in the graph.
pub fn impact_report(
    graph: &AnalysisGraph,
    target: &ANodeId,
    new_signature: Option<&str>,
) -> Option<ImpactReport> {
    let node = graph.get_node(target)?;
    let signature = new_signature
        .map(str::to_string)
        .or_else(|| node.metadata.get("signature").cloned())
        .unwrap_or_else(|| format!("(unchanged) {}", node.name));
    let description = format!("Signature edit to {}", node.qualified_name);

    let mut tasks: Vec<CascadeTaskSummary> =
        generate_cascade(graph, target, &signature, &description)
            .into_iter()
            .map(|task| {
                let file = task
                    .call_sites
                    .first()
                    .map(|s| s.file_path.display().to_string())
                    .unwrap_or_default();
                let mut call_sites: Vec<ImpactCallSite> = task
                    .call_sites
                    .iter()
                    .map(|s| ImpactCallSite {
                        caller: s.caller_name.clone(),
                        file: s.file_path.display().to_string(),
                        line: s.call_span.start_line,
                    })
                    .collect();
                call_sites
                    .sort_by(|a, b| a.line.cmp(&b.line).then_with(|| a.caller.cmp(&b.caller)));
                CascadeTaskSummary {
                    file,
                    instruction: task.instruction,
                    call_sites,
                }
            })
            .collect();
    tasks.sort_by(|a, b| a.file.cmp(&b.file));
    let call_site_count = tasks.iter().map(|t| t.call_sites.len()).sum();

    Some(ImpactReport {
        target: symbol_ref(node),
        new_signature: signature,
        task_count: tasks.len(),
        call_site_count,
        tasks,
    })
}

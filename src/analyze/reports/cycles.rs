use super::{AnalysisGraph, Serialize, SymbolRef, analysis, symbol_ref};

// =============================================================================
// analyze cycles
// =============================================================================

/// One strongly-connected component with ≥ 2 members, or a self-recursive
/// node.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CycleSummary {
    pub size: usize,
    /// `mutualRecursion`, `selfRecursion`, `moduleCycle`, or `mixed`.
    pub kind: String,
    pub members: Vec<SymbolRef>,
}

/// A suggested edge removal that helps break a cycle.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CycleBreakSuggestion {
    pub from: SymbolRef,
    pub to: SymbolRef,
}

/// Result of [`cycles_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CyclesReport {
    pub cycle_count: usize,
    pub cycles: Vec<CycleSummary>,
    pub break_suggestions: Vec<CycleBreakSuggestion>,
}

pub(crate) fn classify_cycle(members: &[SymbolRef]) -> String {
    let all_functions = members.iter().all(|m| m.kind == "function");
    let all_modules = members.iter().all(|m| m.kind == "module");
    if all_functions {
        if members.len() == 1 {
            "selfRecursion".to_string()
        } else {
            "mutualRecursion".to_string()
        }
    } else if all_modules {
        "moduleCycle".to_string()
    } else {
        "mixed".to_string()
    }
}

/// Strongly-connected components of the bridged graph: mutual-recursion
/// clusters, self-recursive functions, and module/import dependency cycles —
/// plus the analysis crate's greedy cycle-break suggestions.
pub fn cycles_report(graph: &AnalysisGraph) -> CyclesReport {
    let clusters = analysis::find_mutual_recursion(graph);
    let mut cycles: Vec<CycleSummary> = clusters
        .into_iter()
        .map(|cluster| {
            let mut members: Vec<SymbolRef> = cluster
                .members
                .iter()
                .filter_map(|id| graph.get_node(id))
                .map(symbol_ref)
                .collect();
            members.sort();
            CycleSummary {
                size: members.len(),
                kind: classify_cycle(&members),
                members,
            }
        })
        .collect();
    cycles.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.members.cmp(&b.members)));

    let mut break_suggestions: Vec<CycleBreakSuggestion> = analysis::cycle_break_suggestions(graph)
        .into_iter()
        .filter_map(|edge| {
            let from = graph.get_node(&edge.from).map(symbol_ref)?;
            let to = graph.get_node(&edge.to).map(symbol_ref)?;
            Some(CycleBreakSuggestion { from, to })
        })
        .collect();
    break_suggestions.sort_by(|a, b| (&a.from, &a.to).cmp(&(&b.from, &b.to)));

    CyclesReport {
        cycle_count: cycles.len(),
        cycles,
        break_suggestions,
    }
}

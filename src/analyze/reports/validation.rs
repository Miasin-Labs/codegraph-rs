use super::*;

// =============================================================================
// analyze validate
// =============================================================================

/// A caller judged incompatible with the proposed signature.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IncompatibleCaller {
    pub symbol: SymbolRef,
    pub reason: String,
}

/// One previewed call site that an edit to the target would touch.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateCallSite {
    pub caller: String,
    pub file: String,
    pub line: u32,
}

/// Result of [`validate_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateReport {
    pub target: SymbolRef,
    pub params_before: usize,
    pub params_after: usize,
    /// True when no caller is judged incompatible.
    pub is_safe: bool,
    pub compatible: Vec<SymbolRef>,
    pub incompatible: Vec<IncompatibleCaller>,
    /// All call sites (resolved + unresolved) an edit would touch.
    pub call_sites: Vec<ValidateCallSite>,
    pub note: String,
}

/// Simulate a signature (arity) change before making it — the engine's
/// `validation::VirtualValidator` judging every direct caller, plus the
/// affected-call-site preview. Returns `None` if `target` is not in the
/// graph.
pub fn validate_report(
    graph: &AnalysisGraph,
    target: &ANodeId,
    params_before: usize,
    params_after: usize,
) -> Option<ValidateReport> {
    let target_node = graph.get_node(target)?;
    let validator = VirtualValidator::new(graph);
    let result = validator.validate_signature_change(target, params_before, params_after);

    let mut compatible: Vec<SymbolRef> = result
        .compatible
        .iter()
        .filter_map(|id| graph.get_node(id))
        .map(symbol_ref)
        .collect();
    compatible.sort();

    let mut incompatible: Vec<IncompatibleCaller> = result
        .incompatible
        .iter()
        .filter_map(|(id, reason)| {
            Some(IncompatibleCaller {
                symbol: graph.get_node(id).map(symbol_ref)?,
                reason: reason.clone(),
            })
        })
        .collect();
    incompatible.sort_by(|a, b| symbol_sort_key(&a.symbol).cmp(&symbol_sort_key(&b.symbol)));

    let mut call_sites: Vec<ValidateCallSite> = validator
        .preview_affected_call_sites(target)
        .into_iter()
        .map(|s| ValidateCallSite {
            caller: s.caller_name,
            file: s.call_span.file.display().to_string(),
            line: s.call_span.start_line,
        })
        .collect();
    call_sites.sort_by(|a, b| (&a.file, a.line, &a.caller).cmp(&(&b.file, b.line, &b.caller)));

    Some(ValidateReport {
        target: symbol_ref(target_node),
        params_before,
        params_after,
        is_safe: result.is_safe,
        compatible,
        incompatible,
        call_sites,
        note: "Verdicts are call-graph-level: the bridge carries no per-call-site argument \
               counts, so an arity change marks every direct caller as needing review, and an \
               unchanged arity validates as safe. Unresolved calls appear in callSites but \
               receive no verdict."
            .to_string(),
    })
}

use super::{
    ANodeKind,
    AnalysisGraph,
    Path,
    Serialize,
    SymbolRef,
    annotate_graph_from_lcov,
    is_placeholder,
    parse_lcov,
    symbol_ref,
    symbol_sort_key,
};

// =============================================================================
// analyze coverage
// =============================================================================

/// One function with its summed LCOV line hits.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoveredFunction {
    pub symbol: SymbolRef,
    /// Total `DA` hit count across the function's line span.
    pub coverage_count: u64,
    pub tested: bool,
}

/// Result of [`coverage_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageReport {
    pub lcov_path: String,
    /// Source files present in the LCOV data.
    pub lcov_files: usize,
    /// Malformed LCOV lines skipped by the parser.
    pub parse_warnings: usize,
    /// Non-placeholder functions in the bridged graph.
    pub functions_total: usize,
    pub functions_tested: usize,
    pub functions_untested: usize,
    /// True when the listing was filtered to untested functions.
    pub untested_only: bool,
    pub truncated: bool,
    pub functions: Vec<CoveredFunction>,
    pub note: String,
}

/// Parse an LCOV file and annotate every `Function` node with
/// `coverage_count`/`coverage_tested` metadata (the engine's coverage
/// contract — the same keys the DSL `untested` operator reads).
/// Returns `(lcov_file_count, parse_warnings)`.
pub fn annotate_coverage(
    graph: &mut AnalysisGraph,
    lcov_path: &Path,
    project_root: &Path,
) -> Result<(usize, usize), String> {
    let file = std::fs::File::open(lcov_path)
        .map_err(|e| format!("cannot read LCOV file {}: {e}", lcov_path.display()))?;
    let (lcov, warnings) = parse_lcov(std::io::BufReader::new(file));
    if lcov.files.is_empty() {
        return Err(format!(
            "{} contains no LCOV coverage records (no SF/DA lines)",
            lcov_path.display()
        ));
    }
    let file_count = lcov.files.len();
    annotate_graph_from_lcov(graph, &lcov, project_root);
    Ok((file_count, warnings))
}

/// Map LCOV line coverage onto the bridged graph and report per-function
/// tested/untested status (engine entry points: `coverage::parse_lcov` +
/// `coverage::annotate_graph_from_lcov`).
pub fn coverage_report(
    graph: &mut AnalysisGraph,
    lcov_path: &Path,
    project_root: &Path,
    untested_only: bool,
    top: usize,
) -> Result<CoverageReport, String> {
    let (lcov_files, parse_warnings) = annotate_coverage(graph, lcov_path, project_root)?;

    let mut functions: Vec<CoveredFunction> = graph
        .nodes_by_kind(ANodeKind::Function)
        .into_iter()
        .filter(|n| !is_placeholder(n))
        .map(|n| {
            let coverage_count = n
                .metadata
                .get("coverage_count")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
            let tested = n
                .metadata
                .get("coverage_tested")
                .map(|v| v == "true")
                .unwrap_or(false);
            CoveredFunction {
                symbol: symbol_ref(n),
                coverage_count,
                tested,
            }
        })
        .collect();

    let functions_total = functions.len();
    let functions_tested = functions.iter().filter(|f| f.tested).count();
    let functions_untested = functions_total - functions_tested;

    // Untested first, then fewest hits, then location — the actionable order.
    functions.sort_by(|a, b| {
        a.tested
            .cmp(&b.tested)
            .then_with(|| a.coverage_count.cmp(&b.coverage_count))
            .then_with(|| symbol_sort_key(&a.symbol).cmp(&symbol_sort_key(&b.symbol)))
    });
    if untested_only {
        functions.retain(|f| !f.tested);
    }
    let truncated = functions.len() > top;
    functions.truncate(top);

    let mut note = "Coverage is line-granular: LCOV DA hit counts are summed over each \
                    function's line span. Annotating coverage also enables the DSL `untested` \
                    operator (`analyze query --lcov <path> '... | untested'`)."
        .to_string();
    if functions_tested == 0 && functions_total > 0 {
        note.push_str(
            " No function matched any covered line — check that the LCOV SF paths correspond \
             to the indexed file paths (relative to the project root).",
        );
    }

    Ok(CoverageReport {
        lcov_path: lcov_path.display().to_string(),
        lcov_files,
        parse_warnings,
        functions_total,
        functions_tested,
        functions_untested,
        untested_only,
        truncated,
        functions,
        note,
    })
}

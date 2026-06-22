use super::*;

// =============================================================================
// Shared shapes
// =============================================================================

/// A symbol reference rendered into every report.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolRef {
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
}

pub(crate) fn kind_label(kind: ANodeKind) -> String {
    match kind {
        ANodeKind::Function => "function".to_string(),
        ANodeKind::Struct => "struct".to_string(),
        ANodeKind::Enum => "enum".to_string(),
        ANodeKind::Module => "module".to_string(),
        ANodeKind::Trait => "trait".to_string(),
        other => format!("{other:?}").to_lowercase(),
    }
}

pub(crate) fn symbol_ref(node: &ANodeData) -> SymbolRef {
    SymbolRef {
        name: node.name.clone(),
        qualified_name: node.qualified_name.clone(),
        kind: kind_label(node.kind),
        file: node.file_path.display().to_string(),
        line: node.span.start_line,
    }
}

pub(crate) fn is_placeholder(node: &ANodeData) -> bool {
    node.file_path.as_os_str() == UNRESOLVED_FILE
}

pub(crate) fn edge_kind_label(kind: &AEdgeKind) -> &'static str {
    match kind {
        AEdgeKind::Calls => "calls",
        AEdgeKind::UnresolvedCall(_) => "unresolvedCall",
        AEdgeKind::UsesType => "usesType",
        AEdgeKind::References => "references",
        AEdgeKind::Contains => "contains",
        AEdgeKind::Implements => "implements",
        AEdgeKind::ExternalCall(..) => "externalCall",
        AEdgeKind::Extends => "extends",
        AEdgeKind::Returns => "returns",
        AEdgeKind::TypeOf => "typeOf",
    }
}
// =============================================================================
// analyze slice|taint --source (engine CPG report façade)
// =============================================================================

/// Byte-offset presence over the bridged graph's non-placeholder `Function`
/// nodes — the cheap scan behind the `--source` honesty notes (no file IO,
/// no parsing). Returns `(total, missing_byte_range)`; `0..0` is the
/// bridge's documented "unknown" value for pre-v5 index rows.
pub(crate) fn function_byte_presence(graph: &AnalysisGraph) -> (usize, usize) {
    let mut total = 0usize;
    let mut missing = 0usize;
    for node in graph.nodes_by_kind(ANodeKind::Function) {
        if is_placeholder(node) {
            continue;
        }
        total += 1;
        let range = &node.span.byte_range;
        if range.start == 0 && range.end == 0 {
            missing += 1;
        }
    }
    (total, missing)
}

/// True when the process runs from `workspace_root` (the engine façade reads
/// the bridged graph's project-relative paths against the cwd). Unknowable
/// states (canonicalize failures) report `true` so no spurious warning is
/// emitted.
fn runs_from_workspace_root(workspace_root: &Path) -> bool {
    let cwd = std::env::current_dir().and_then(|d| d.canonicalize());
    let root = workspace_root.canonicalize();
    match (cwd, root) {
        (Ok(cwd), Ok(root)) => cwd == root,
        _ => true,
    }
}

/// Byte-offset coverage embedded in `--source` reports so consumers can
/// judge how much value-level fidelity backed the annotated text.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceReportCoverage {
    /// Non-placeholder `Function` nodes in the bridged graph.
    pub functions_total: usize,
    /// Functions whose index rows carry no byte offsets (indexed before
    /// schema v5) — they contribute no value-level dataflow hops.
    pub functions_missing_byte_range: usize,
}

/// Shared honesty note for the `--source` reports: states what the
/// annotations are, whether value-level fidelity is available (and how to
/// get it back when it is not), and warns when the process cwd is not the
/// project root (the engine façade resolves the graph's project-relative
/// paths against the cwd).
pub(crate) fn source_report_note(
    workspace_root: &Path,
    lead: &str,
    coverage: &SourceReportCoverage,
) -> String {
    let mut note = format!(
        "{lead} Annotations are `name (file:line)` from the indexed spans and work on any index."
    );
    if coverage.functions_total > 0
        && coverage.functions_missing_byte_range == coverage.functions_total
    {
        note.push_str(
            " Value-level fidelity is unavailable: no indexed function carries byte offsets \
             (indexed before schema v5), so the underlying points-to oracle sees no dataflow \
             — re-index (\"codegraph index\") to enable it.",
        );
    } else if coverage.functions_missing_byte_range > 0 {
        note.push_str(&format!(
            " {} of {} functions lack byte offsets (indexed before schema v5) and contribute \
             no value-level hops — re-index (\"codegraph index\") to include them.",
            coverage.functions_missing_byte_range, coverage.functions_total
        ));
    } else {
        note.push_str(" Value-level fidelity rides the index's byte offsets (schema v5).");
    }
    if !runs_from_workspace_root(workspace_root) {
        note.push_str(
            " Source files are resolved relative to the current working directory; run from \
             the project root for value-level fidelity (line annotations are unaffected).",
        );
    }
    note
}

pub(crate) fn symbol_sort_key(s: &SymbolRef) -> (&String, u32, &String) {
    (&s.file, s.line, &s.name)
}

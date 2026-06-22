use super::{Serialize, SymbolRef};

// =============================================================================
// analyze query (pipe-based DSL)
// =============================================================================

/// An edge between two result nodes, by qualified name.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryEdge {
    pub from: String,
    pub to: String,
    /// Edge kind as described by the query engine (e.g. `Calls`, `Contains`).
    pub kind: String,
}

/// One why-provenance step: which DSL operator put a node into the result.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WhyStep {
    /// The DSL operator that produced/kept the node.
    pub op: String,
    /// Qualified names of the previous working-set nodes that led here
    /// (empty for seed selectors like `fn("x")`).
    pub predecessors: Vec<String>,
    /// Pipeline stage at which the node entered the working set.
    pub stage: usize,
}

/// Why-provenance for one result node.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WhyEntry {
    pub symbol: SymbolRef,
    pub steps: Vec<WhyStep>,
}

/// Result of [`query_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryReport {
    pub query: String,
    /// Result rows, sorted by file/line/name. For path queries the path
    /// *order* lives in `edges` (hops are emitted in path order).
    pub nodes: Vec<SymbolRef>,
    pub node_count: usize,
    pub total_before_truncation: usize,
    pub truncated: bool,
    /// Edges between result nodes.
    pub edges: Vec<QueryEdge>,
    /// Engine metadata lines: aggregation scalars (`scalar = N`,
    /// `bool = …`), SCC/cluster/community descriptions, etc.
    pub metadata: Vec<String>,
    /// Nodes at which a traversal detected a cycle.
    pub cycles_detected: Vec<SymbolRef>,
    /// Why-provenance — present when requested *and* the query shape is
    /// traceable (aggregation queries are not; see [`query_report`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<Vec<WhyEntry>>,
    /// Source-level guarding conditions — present only when the query
    /// contains the `preconditions` operator and a workspace root was
    /// supplied (see [`query_report_with_sources`]). Never silently empty:
    /// the section's `note` explains absent or partial extraction.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preconditions: Option<PreconditionsSection>,
}

/// Source-level enrichment of the DSL `preconditions` operator: the actual
/// guarding conditions (`if` / `match` / `while` / `for` / `loop`) wrapped
/// around each call site between result nodes, extracted by re-parsing the
/// on-disk sources (engine entry point: `predicates::extract_predicates`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreconditionsSection {
    /// Number of guarded call sites found (== `guards.len()`).
    pub guarded_call_count: usize,
    pub guards: Vec<PreconditionGuard>,
    /// Honesty note: reading order of the conditions, plus any extraction
    /// gaps (pre-v5 byte offsets, non-Rust call sites, unreadable files).
    pub note: String,
}

/// One guarded call site between two result nodes.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreconditionGuard {
    pub caller: SymbolRef,
    pub callee: String,
    /// Call-site location (the caller's file).
    pub file: String,
    pub line: u32,
    /// Guarding conditions, outermost first (evaluation order: the first
    /// entry is the outermost gate control flow passed through).
    pub conditions: Vec<String>,
}

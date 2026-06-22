use super::{CycleSummary, Serialize, SymbolRef};

/// Where the diff base came from.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffBaseDescriptor {
    /// `cache` (stale current generation), `cache-prev` (rotated previous
    /// generation), or `file` (explicit `--base <path>`).
    pub source: String,
    /// Hex index fingerprint of the base, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_fingerprint: Option<String>,
}

/// One node present in both states whose structure differs.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangedNode {
    pub symbol: SymbolRef,
    /// What differs: `spanLines`, `byteLength`, `signature`, `visibility`,
    /// `fields`, `variants`, `accessedFields`, `async`, `exported`.
    pub reasons: Vec<String>,
}

/// An edge present in exactly one of the two states.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffEdge {
    pub from: String,
    pub to: String,
    pub kind: String,
}

/// Complexity before/after for one changed function.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangedFunctionDelta {
    pub symbol: SymbolRef,
    pub lines_before: u32,
    pub lines_after: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cyclomatic_before: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cyclomatic_after: Option<u32>,
    /// Present only when both ends were measured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cyclomatic_delta: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cognitive_before: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cognitive_after: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cognitive_delta: Option<i64>,
}

/// Impact section of [`DiffReport`]: what the delta can break.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeltaImpact {
    /// BFS depth walked from each delta node along incoming edges.
    pub depth: usize,
    /// Impacted symbols found (before truncation; delta nodes excluded).
    pub impacted_count: usize,
    pub truncated: bool,
    pub nodes: Vec<SymbolRef>,
}

/// Result of [`diff_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffReport {
    pub base: DiffBaseDescriptor,
    pub nodes_added_count: usize,
    pub nodes_removed_count: usize,
    pub nodes_changed_count: usize,
    pub nodes_added: Vec<SymbolRef>,
    pub nodes_removed: Vec<SymbolRef>,
    pub nodes_changed: Vec<ChangedNode>,
    pub edges_added_count: usize,
    pub edges_removed_count: usize,
    pub edges_added: Vec<DiffEdge>,
    pub edges_removed: Vec<DiffEdge>,
    /// True when any listing above was capped at `top`.
    pub truncated: bool,
    /// Changed/added `Function` nodes with complexity before/after.
    pub changed_functions: Vec<ChangedFunctionDelta>,
    /// SCC clusters present now but not in the base.
    pub new_cycle_count: usize,
    pub new_cycles: Vec<CycleSummary>,
    /// SCC clusters present in the base but not anymore.
    pub resolved_cycle_count: usize,
    pub impact: DeltaImpact,
    pub note: String,
}

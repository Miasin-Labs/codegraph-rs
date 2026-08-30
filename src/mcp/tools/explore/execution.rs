/// Internal retrieval limits for one explore call.
///
/// These limits are deliberately independent of the public `maxFiles` option:
/// `maxFiles` controls only how many ranked source files are rendered, never
/// which symbols or literal matches are discovered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::mcp::tools::explore) struct ExploreExecutionBudget {
    pub search_limit: usize,
    pub traversal_depth: u32,
    pub max_nodes: usize,
    pub named_seed_token_limit: usize,
    pub literal_scan: LiteralScanBudget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::mcp::tools::explore) struct LiteralScanBudget {
    pub max_files: usize,
    pub max_bytes: u64,
}

impl Default for ExploreExecutionBudget {
    fn default() -> Self {
        Self {
            search_limit: 8,
            traversal_depth: 3,
            max_nodes: 200,
            named_seed_token_limit: 8,
            literal_scan: LiteralScanBudget::default(),
        }
    }
}

impl Default for LiteralScanBudget {
    fn default() -> Self {
        Self {
            max_files: 2_000,
            max_bytes: 50_000_000,
        }
    }
}

//! Explore budget and environment knobs.

/// Maximum output length to prevent context bloat (characters)
/// Optional server-side destructive output cap, in chars. UNSET (the
/// default) means the server NEVER truncates tool output — the host owns
/// inline-size policy (Claude Code rejects results over
/// `MAX_MCP_OUTPUT_TOKENS` ≈ 25K tokens ≈ 100K chars; jfc head/tail-trims at
/// 50KB and spills >400KB to disk). Set `CODEGRAPH_MAX_OUTPUT_CHARS=N` to
/// re-enable a cap for hosts that need one; 0/unset disables.
pub(in crate::mcp::tools) fn output_char_cap() -> Option<usize> {
    let v = std::env::var("CODEGRAPH_MAX_OUTPUT_CHARS").ok()?;
    let n: usize = v.trim().parse().ok()?;
    (n > 0).then_some(n)
}

/// Calculate the recommended number of codegraph_explore calls based on project size.
/// Larger codebases need more exploration calls to cover their surface area,
/// but smaller ones should use fewer to avoid unnecessary overhead.
pub fn get_explore_budget(file_count: u64) -> u32 {
    if file_count < 500 {
        return 1;
    }
    if file_count < 5000 {
        return 2;
    }
    if file_count < 15000 {
        return 3;
    }
    if file_count < 25000 {
        return 4;
    }
    5
}

/// Adaptive output budget for `codegraph_explore`, scaled to project size.
///
/// Smaller codebases get a tighter total cap, fewer default files, smaller
/// per-file cap, and tighter clustering — so a focused query on a 100-file
/// project doesn't dump a whole file's worth of source into the agent's
/// context. Larger codebases keep the generous defaults because the
/// agent's native discovery cost (grep + find + many Reads) genuinely
/// dwarfs a fat explore call at that scale.
///
/// Tier breakpoints mirror `get_explore_budget` so a project sits in the
/// same tier across both knobs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExploreOutputBudget {
    /// Hard cap on total output characters.
    pub max_output_chars: usize,
    /// Default `maxFiles` when the caller didn't specify one.
    pub default_max_files: usize,
    /// Cap on contiguous source returned per file (across all its clusters).
    pub max_chars_per_file: usize,
    /// Cluster gap threshold in lines — tighter clustering on small projects.
    pub gap_threshold: i64,
    /// Max symbols listed in the per-file header (`#### path — sym(kind), ...`).
    pub max_symbols_in_file_header: usize,
    /// Max edges shown per relationship kind in the Relationships section.
    pub max_edges_per_relationship_kind: usize,
    /// Include the "Relationships" section.
    pub include_relationships: bool,
    /// Include the "Additional relevant files (not shown)" trailing list.
    pub include_additional_files: bool,
    /// Include the "Complete source code is included above…" reminder.
    pub include_completeness_signal: bool,
    /// Include the explore-budget reminder at the end.
    pub include_budget_note: bool,
    /// Hard-drop test/spec/icon/i18n files from the relevant-file set unless
    /// the query itself mentions tests.
    pub exclude_low_value_files: bool,
}

pub fn get_explore_output_budget(file_count: u64) -> ExploreOutputBudget {
    // Tiered budget, scaled to project size. The budget is a CEILING (relevance
    // still gates WHAT is included), and it MUST stay under the agent's INLINE
    // tool-result cap (~25K chars). Invariant: a larger tier must never get a
    // smaller `max_chars_per_file` than a smaller tier.
    if file_count < 150 {
        return ExploreOutputBudget {
            max_output_chars: 13000,
            default_max_files: 4,
            max_chars_per_file: 3800,
            gap_threshold: 7,
            max_symbols_in_file_header: 5,
            max_edges_per_relationship_kind: 4,
            include_relationships: false,
            include_additional_files: false,
            include_completeness_signal: false,
            include_budget_note: false,
            exclude_low_value_files: true,
        };
    }
    if file_count < 500 {
        return ExploreOutputBudget {
            max_output_chars: 18000,
            default_max_files: 5,
            max_chars_per_file: 3800,
            gap_threshold: 8,
            max_symbols_in_file_header: 6,
            max_edges_per_relationship_kind: 6,
            include_relationships: false,
            include_additional_files: false,
            include_completeness_signal: false,
            include_budget_note: false,
            exclude_low_value_files: true,
        };
    }
    if file_count < 5000 {
        return ExploreOutputBudget {
            max_output_chars: 24000,
            default_max_files: 8,
            max_chars_per_file: 6500,
            gap_threshold: 12,
            max_symbols_in_file_header: 10,
            max_edges_per_relationship_kind: 10,
            include_relationships: true,
            include_additional_files: true,
            include_completeness_signal: true,
            include_budget_note: true,
            exclude_low_value_files: false,
        };
    }
    if file_count < 15000 {
        return ExploreOutputBudget {
            max_output_chars: 24000,
            default_max_files: 8,
            max_chars_per_file: 7000,
            gap_threshold: 15,
            max_symbols_in_file_header: 15,
            max_edges_per_relationship_kind: 15,
            include_relationships: true,
            include_additional_files: true,
            include_completeness_signal: true,
            include_budget_note: true,
            exclude_low_value_files: false,
        };
    }
    ExploreOutputBudget {
        max_output_chars: 24000,
        default_max_files: 8,
        max_chars_per_file: 7000,
        gap_threshold: 15,
        max_symbols_in_file_header: 15,
        max_edges_per_relationship_kind: 15,
        include_relationships: true,
        include_additional_files: true,
        include_completeness_signal: true,
        include_budget_note: true,
        exclude_low_value_files: false,
    }
}

/// Whether `codegraph_explore` should prefix source lines with their line
/// numbers (cat -n style: `<num>\t<code>`). Defaults ON. Set
/// `CODEGRAPH_EXPLORE_LINENUMS=0` to disable.
pub(in crate::mcp::tools) fn explore_line_numbers_enabled() -> bool {
    std::env::var("CODEGRAPH_EXPLORE_LINENUMS")
        .map(|v| v != "0")
        .unwrap_or(true)
}

/// Adaptive explore sizing (default ON). Set `CODEGRAPH_ADAPTIVE_EXPLORE=0`
/// to disable.
pub(in crate::mcp::tools) fn adaptive_explore_enabled() -> bool {
    match std::env::var("CODEGRAPH_ADAPTIVE_EXPLORE") {
        Ok(v) => v != "0" && v != "false",
        Err(_) => true,
    }
}

pub(in crate::mcp::tools) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

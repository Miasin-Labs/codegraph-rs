//! Shared MCP output formatting and budget helpers.

mod budget;
mod kinds;
mod ordered;
mod regexes;
mod results;
mod stale;
mod symbols;
mod tool_text;

pub use budget::{ExploreOutputBudget, get_explore_budget, get_explore_output_budget};
pub(in crate::mcp::tools) use budget::{
    adaptive_explore_enabled,
    explore_line_numbers_enabled,
    now_ms,
    output_char_cap,
};
pub(in crate::mcp::tools) use kinds::{is_callable_kind, is_container_node_kind};
pub(in crate::mcp::tools) use ordered::{
    FlowInfo,
    OrderedNodeMap,
    SynthNote,
    ordered_nodes_from_subgraph,
};
pub(in crate::mcp::tools) use regexes::{
    EXT_STRIP_RE,
    FILE_EXT_RE,
    LEADING_DOT_SLASH_RE,
    LOW_VALUE_RES,
    MAX_INPUT_LENGTH,
    MAX_PATH_LENGTH,
    QUAL_DOT_SPLIT_RE,
    QUALIFIER_SPLIT_RE,
    QUERY_MENTIONS_TESTS_RE,
    RUST_PATH_PREFIXES,
    TEST_PATH_DIR_RE,
    TEST_PATH_EXT_RE,
    TOKEN_RE,
    TOKEN_SPLIT_RE,
    TYPE_TOKEN_RE,
};
pub use stale::{format_stale_banner, format_stale_footer};
pub(in crate::mcp::tools) use symbols::{
    display_symbol,
    extract_symbol_tokens,
    floor_char_boundary,
    is_low_value,
    is_qualified_token,
    is_test_path,
    last_qualifier_part,
    locale_cmp,
    num_or,
    number_source_lines,
    resolve_path,
    slice_lines,
    to_locale_string,
    truthy_meta_string,
};

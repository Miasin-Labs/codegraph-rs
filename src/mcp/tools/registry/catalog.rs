//! Ordered MCP tool catalog assembly.

use super::super::schema::ToolDefinition;
use super::admin::{push_files_tool, push_status_tool};
use super::explore::push_explore_tool;
use super::insight::{push_diagnostics_tool, push_history_tool, push_tests_tool};
use super::lookup::{push_callee_tools, push_impact_tool, push_node_tool, push_search_tool};
use super::memory::push_recall_tool;
use super::navigation::{push_arch_tool, push_paths_tool, push_xref_tool};
use super::text::push_grep_tool;

/// All CodeGraph MCP tools (mirrors the TS `tools` array, same order).
pub fn tools() -> Vec<ToolDefinition> {
    let mut out = Vec::with_capacity(14);
    push_search_tool(&mut out);
    push_callee_tools(&mut out);
    push_impact_tool(&mut out);
    push_node_tool(&mut out);
    push_explore_tool(&mut out);
    push_status_tool(&mut out);
    push_files_tool(&mut out);
    push_history_tool(&mut out);
    push_tests_tool(&mut out);
    push_diagnostics_tool(&mut out);
    push_grep_tool(&mut out);
    push_arch_tool(&mut out);
    push_xref_tool(&mut out);
    push_paths_tool(&mut out);
    push_recall_tool(&mut out);
    out
}

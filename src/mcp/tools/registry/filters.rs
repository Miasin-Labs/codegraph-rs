//! Static registry filtering through CODEGRAPH_MCP_TOOLS.

use std::collections::HashSet;

use super::super::schema::ToolDefinition;
use super::catalog::tools;

pub(in crate::mcp::tools) fn short_tool_name(name: &str) -> &str {
    name.strip_prefix("codegraph_").unwrap_or(name)
}

/// Optional allowlist of exposed tools, parsed from the CODEGRAPH_MCP_TOOLS
/// env var (comma-separated short names). Unset/empty → every tool exposed.
pub(in crate::mcp::tools) fn tool_allowlist() -> Option<HashSet<String>> {
    let raw = std::env::var("CODEGRAPH_MCP_TOOLS").ok()?;
    if raw.trim().is_empty() {
        return None;
    }
    let set: HashSet<String> = raw
        .split(',')
        .map(|s| short_tool_name(s.trim()).to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if set.is_empty() { None } else { Some(set) }
}

/// Allowlist-filtered tool definitions WITHOUT an engine — the static surface
/// the proxy answers `tools/list` with before any project is open.
pub fn get_static_tools() -> Vec<ToolDefinition> {
    match tool_allowlist() {
        Some(allow) => tools()
            .into_iter()
            .filter(|t| allow.contains(short_tool_name(&t.name)))
            .collect(),
        None => tools(),
    }
}

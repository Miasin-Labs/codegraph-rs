//! Dynamic MCP tool listing and allowlist handling.

use super::super::format::{get_explore_budget, to_locale_string};
use super::super::registry::{short_tool_name, tool_allowlist, tools};
use super::super::schema::ToolDefinition;
use super::ToolHandler;

impl ToolHandler {
    pub(in crate::mcp::tools::context) fn is_tool_allowed(&self, name: &str) -> bool {
        match tool_allowlist() {
            Some(allow) => allow.contains(short_tool_name(name)),
            None => true,
        }
    }

    /// Get tool definitions with dynamic descriptions based on project size.
    pub fn get_tools(&self) -> Vec<ToolDefinition> {
        let allow = tool_allowlist();
        let mut visible: Vec<ToolDefinition> = match &allow {
            Some(set) => tools()
                .into_iter()
                .filter(|t| set.contains(short_tool_name(&t.name)))
                .collect(),
            None => tools(),
        };
        let cg_ref = self.cg.borrow();
        let Some(cg) = cg_ref.as_ref() else {
            return visible;
        };

        let Ok(stats) = cg.get_stats() else {
            return visible;
        };
        let budget = get_explore_budget(stats.file_count);

        // Tiny-repo tool gating: on projects under TINY_REPO_FILE_THRESHOLD
        // files, only expose the core tools — the omitted tools reduce to one
        // grep at this scale (see the TS source for the full A/B rationale).
        const TINY_REPO_FILE_THRESHOLD: u64 = 500;
        if stats.file_count < TINY_REPO_FILE_THRESHOLD {
            visible.retain(|t| {
                matches!(
                    t.name.as_str(),
                    "codegraph_explore" | "codegraph_search" | "codegraph_node"
                )
            });
        }

        for tool in &mut visible {
            if tool.name == "codegraph_explore" {
                tool.description = format!(
                    "{} Budget: make at most {} calls for this project ({} files indexed).",
                    tool.description,
                    budget,
                    to_locale_string(stats.file_count)
                );
            }
        }
        visible
    }
}

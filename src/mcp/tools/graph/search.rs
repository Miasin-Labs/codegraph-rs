//! Search support for graph MCP tools.

use serde_json::{Map, Value};

use super::super::context::ToolHandler;
use super::super::format::num_or;
use super::super::output::SearchOutput;
use super::super::schema::ToolResult;
use crate::error::Result;
use crate::extraction::is_generated_file;
use crate::types::{NodeKind, SearchOptions};
use crate::utils::clamp;

impl ToolHandler {
    pub(in crate::mcp::tools) fn handle_search(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolResult> {
        let query = match self.validate_string(args.get("query"), "query") {
            Ok(q) => q,
            Err(r) => return Ok(r),
        };

        let cg = self.get_code_graph(args.get("projectPath").and_then(|v| v.as_str()))?;
        let kind = args
            .get("kind")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let raw_limit = num_or(args, "limit", 10.0);
        let limit = clamp(raw_limit, 1.0, 100.0) as usize;

        let kinds: Option<Vec<NodeKind>> = match kind {
            Some("type") => Some(vec![NodeKind::TypeAlias]),
            Some(k) => match k.parse::<NodeKind>() {
                Ok(nk) => Some(vec![nk]),
                Err(_) => {
                    let output =
                        SearchOutput::new(query.clone(), kind.map(str::to_string), limit, &[]);
                    return self
                        .structured_result(&format!("Search results: 0 for `{query}`"), &output);
                }
            },
            None => None,
        };

        let results = cg.search_nodes(
            &query,
            Some(&SearchOptions {
                limit: Some(limit),
                kinds,
                ..Default::default()
            }),
        )?;

        let mut ranked = results;
        ranked.sort_by_key(|r| {
            if is_generated_file(&r.node.file_path) {
                1
            } else {
                0
            }
        });

        let formatted = self.format_search_results(&ranked);
        let output = SearchOutput::new(query, kind.map(str::to_string), limit, &ranked);
        self.structured_result(&self.truncate_output(&formatted), &output)
    }

    // =========================================================================
    // codegraph_callers / codegraph_callees
}

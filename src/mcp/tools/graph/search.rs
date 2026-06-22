//! Search support for graph MCP tools.

use serde_json::{Map, Value};

use super::super::context::ToolHandler;
use super::super::format::num_or;
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

        // TS passes the raw kind string through; an unknown kind (e.g. "type")
        // matches no rows. NodeKind can't represent it, so short-circuit the
        // same empty-result outcome.
        let kinds: Option<Vec<NodeKind>> = match kind {
            Some(k) => match k.parse::<NodeKind>() {
                Ok(nk) => Some(vec![nk]),
                Err(_) => {
                    return Ok(self.text_result(&format!("No results found for \"{query}\"")));
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

        if results.is_empty() {
            return Ok(self.text_result(&format!("No results found for \"{query}\"")));
        }

        // Down-rank generated files within the FTS-returned set. Stable.
        let mut ranked = results;
        ranked.sort_by_key(|r| {
            if is_generated_file(&r.node.file_path) {
                1
            } else {
                0
            }
        });

        let formatted = self.format_search_results(&ranked);
        Ok(self.text_result(&self.truncate_output(&formatted)))
    }

    // =========================================================================
    // codegraph_callers / codegraph_callees
}

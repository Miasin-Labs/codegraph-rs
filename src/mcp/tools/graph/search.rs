//! Search support for graph MCP tools.

use serde_json::{Map, Value};

use super::super::context::ToolHandler;
use super::super::format::{mcp_output_budget, num_or};
use super::super::output::SearchOutput;
use super::super::schema::ToolResult;
use crate::error::Result;
use crate::types::{NodeKind, SearchOptions};
use crate::utils::clamp;

/// `query` accepts one name or a list of them. Agents batch symbol lookups
/// through grep alternations (`a|b|c`) when a tool takes a single name, so the
/// list form is the shape they already reach for.
fn batch_queries(args: &Map<String, Value>) -> Option<Vec<String>> {
    let items = args
        .get("symbols")
        .and_then(Value::as_array)
        .or_else(|| args.get("query").and_then(Value::as_array))?;
    let mut names = Vec::new();
    // A lone `query` alongside `symbols` is one more name, not a conflict.
    if let Some(query) = args.get("query").and_then(Value::as_str) {
        names.push(query.trim().to_string());
    }
    names.extend(
        items
            .iter()
            .filter_map(Value::as_str)
            .map(|name| name.trim().to_string()),
    );
    names.retain(|name| !name.is_empty());
    names.dedup();
    (!names.is_empty()).then_some(names)
}

impl ToolHandler {
    pub(in crate::mcp::tools) fn handle_search(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolResult> {
        if let Some(paths) = super::search_projects::project_paths(args) {
            return self.handle_search_projects(args, paths);
        }
        if let Some(queries) = batch_queries(args) {
            return self.handle_search_batch(args, queries);
        }
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
                    return self.structured_result(
                        &format!("Search results: 0 for `{query}`"),
                        &SearchOutput::new(&[]),
                    );
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
        let paths = ranked
            .iter()
            .map(|result| result.node.file_path.clone())
            .collect::<Vec<_>>();
        let generated = cg.generated_file_predicate(&paths)?;
        ranked.sort_by_key(|result| generated.is_generated(&result.node.file_path));

        let formatted = self.format_search_results(&ranked);
        let mut output = SearchOutput::new(&ranked);
        output.fit_to(mcp_output_budget());
        self.structured_result(&self.truncate_output(&formatted), &output)
    }

    /// Run one search per name and return the hits grouped by name. A symbol
    /// found by several names is reported once, under the first that found it.
    fn handle_search_batch(
        &self,
        args: &Map<String, Value>,
        queries: Vec<String>,
    ) -> Result<ToolResult> {
        const MAX_QUERIES: usize = 25;

        let cg = self.get_code_graph(args.get("projectPath").and_then(|v| v.as_str()))?;
        let kind = args
            .get("kind")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let kinds: Option<Vec<NodeKind>> = match kind {
            Some("type") => Some(vec![NodeKind::TypeAlias]),
            Some(k) => match k.parse::<NodeKind>() {
                Ok(nk) => Some(vec![nk]),
                Err(_) => None,
            },
            None => None,
        };
        let per_query = clamp(num_or(args, "limit", 10.0), 1.0, 100.0) as usize;
        let queries: Vec<String> = queries.into_iter().take(MAX_QUERIES).collect();

        let mut hits: Vec<(String, crate::types::SearchResult)> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut unmatched = Vec::new();
        let mut lines = Vec::new();
        for name in &queries {
            let found = cg.search_nodes(
                name,
                Some(&SearchOptions {
                    limit: Some(per_query),
                    kinds: kinds.clone(),
                    ..Default::default()
                }),
            )?;
            let fresh = found
                .into_iter()
                .filter(|result| seen.insert(result.node.id.clone()))
                .collect::<Vec<_>>();
            lines.push(format!(
                "### `{name}` - {} match{}",
                fresh.len(),
                if fresh.len() == 1 { "" } else { "es" }
            ));
            lines.push(if fresh.is_empty() {
                "_no matches_".to_string()
            } else {
                self.format_search_results(&fresh)
            });
            if fresh.is_empty() {
                unmatched.push(name.clone());
            }
            hits.extend(fresh.into_iter().map(|result| (name.clone(), result)));
        }

        let text = format!(
            "Search results: {} across {} name{}\n\n{}",
            hits.len(),
            queries.len(),
            if queries.len() == 1 { "" } else { "s" },
            lines.join("\n")
        );
        let mut output = SearchOutput::new_batch(hits, unmatched);
        output.fit_to(mcp_output_budget());
        self.structured_result(&self.truncate_output(&text), &output)
    }

    // =========================================================================
    // codegraph_callers / codegraph_callees
}

//! `codegraph_search` across several indexed projects in one call.
//!
//! Each index is its own SQLite graph, so an agent working across repos had
//! to issue one call per `projectPath` (1,000 such calls in the mined
//! sessions). This fans the ordinary search — including the `symbols` batch
//! form — out over `projectPaths` and tags every hit with its project.

use serde_json::{Map, Value, json};

use super::super::context::ToolHandler;
use super::super::format::{json_len, mcp_output_budget, rows_within_budget};
use super::super::schema::ToolResult;
use crate::error::Result;

const MAX_PROJECTS: usize = 8;

pub(in crate::mcp::tools) fn project_paths(args: &Map<String, Value>) -> Option<Vec<String>> {
    let items = args.get("projectPaths")?.as_array()?;
    let mut paths: Vec<String> = items
        .iter()
        .filter_map(Value::as_str)
        .map(|path| path.trim().to_string())
        .filter(|path| !path.is_empty())
        .collect();
    paths.dedup();
    (!paths.is_empty()).then_some(paths)
}

impl ToolHandler {
    pub(in crate::mcp::tools) fn handle_search_projects(
        &self,
        args: &Map<String, Value>,
        paths: Vec<String>,
    ) -> Result<ToolResult> {
        let paths: Vec<String> = paths.into_iter().take(MAX_PROJECTS).collect();
        let mut sections = Vec::new();
        let mut results = Vec::new();
        let mut failed = Vec::new();
        let mut truncated = false;
        // A batch name is unmatched only if no project matched it.
        let mut unmatched: Option<Vec<Value>> = None;

        for path in &paths {
            let path_value = Value::String(path.clone());
            if let Err(error) = self.validate_optional_path(Some(&path_value), "projectPaths") {
                sections.push(format!("## {path}\n\n{}", error_text(&error)));
                failed.push(json!({ "project": path, "message": error_text(&error) }));
                continue;
            }
            let mut single = args.clone();
            single.remove("projectPaths");
            single.insert("projectPath".into(), path_value);
            let result = match self.handle_search(&single) {
                Ok(result) => result,
                Err(error) => {
                    sections.push(format!("## {path}\n\n_search failed: {error}_"));
                    failed.push(json!({ "project": path, "message": error.to_string() }));
                    continue;
                }
            };
            sections.push(format!("## {path}\n\n{}", error_text(&result)));
            let Some(payload) = result.structured_content.as_ref() else {
                continue;
            };
            if payload.get("kind").and_then(Value::as_str) != Some("search") {
                failed.push(json!({ "project": path, "message": error_text(&result) }));
                continue;
            }
            truncated |= payload["truncated"].as_bool().unwrap_or(false);
            let names = payload["unmatched"].as_array().cloned().unwrap_or_default();
            unmatched = Some(match unmatched {
                None => names,
                Some(previous) => previous
                    .into_iter()
                    .filter(|name| names.contains(name))
                    .collect(),
            });
            for hit in payload["results"].as_array().into_iter().flatten() {
                let mut hit = hit.clone();
                hit["project"] = Value::String(path.clone());
                results.push(hit);
            }
        }

        let mut payload = json!({ "schemaVersion": 2, "kind": "search", "results": [] });
        let unmatched = unmatched.unwrap_or_default();
        if !unmatched.is_empty() {
            payload["unmatched"] = Value::Array(unmatched);
        }
        if !failed.is_empty() {
            payload["failedProjects"] = Value::Array(failed);
        }
        payload["truncated"] = Value::Bool(true);
        let keep = rows_within_budget(
            mcp_output_budget(),
            json_len(&payload),
            results.iter().map(json_len),
        );
        truncated |= keep < results.len();
        results.truncate(keep);
        payload["results"] = Value::Array(results);
        if let (false, Some(object)) = (truncated, payload.as_object_mut()) {
            object.remove("truncated");
        }
        let text = self.truncate_output(&sections.join("\n\n"));
        self.structured_result(&text, &payload)
    }
}

fn error_text(result: &ToolResult) -> String {
    result
        .content
        .iter()
        .map(|content| content.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

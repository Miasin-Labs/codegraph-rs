//! `codegraph_search` across several indexed projects in one call.
//!
//! Each index is its own SQLite graph, so an agent working across repos had
//! to issue one call per `projectPath` (1,000 such calls in the mined
//! sessions). This fans the ordinary search — including the `symbols` batch
//! form — out over `projectPaths` and tags every hit with its project.

use serde_json::{Map, Value, json};

use super::super::context::ToolHandler;
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
        let mut queries: Option<Value> = None;
        let mut query = String::new();
        let mut limit = Value::Null;

        for path in &paths {
            let path_value = Value::String(path.clone());
            if let Err(error) = self.validate_optional_path(Some(&path_value), "projectPaths") {
                sections.push(format!("## {path}\n\n{}", error_text(&error)));
                continue;
            }
            let mut single = args.clone();
            single.remove("projectPaths");
            single.insert("projectPath".into(), path_value);
            let result = match self.handle_search(&single) {
                Ok(result) => result,
                Err(error) => {
                    sections.push(format!("## {path}\n\n_search failed: {error}_"));
                    continue;
                }
            };
            sections.push(format!("## {path}\n\n{}", error_text(&result)));
            let Some(payload) = result.structured_content.as_ref() else {
                continue;
            };
            if payload.get("kind").and_then(Value::as_str) != Some("search") {
                continue;
            }
            query = payload["query"].as_str().unwrap_or_default().to_string();
            limit = payload["limit"].clone();
            if queries.is_none() {
                queries = payload.get("queries").cloned();
            }
            for hit in payload["results"].as_array().into_iter().flatten() {
                let mut hit = hit.clone();
                hit["project"] = Value::String(path.clone());
                results.push(hit);
            }
        }

        let mut payload = json!({
            "schemaVersion": 1,
            "kind": "search",
            "query": query,
            "projects": paths,
            "limit": if limit.is_null() { Value::from(10) } else { limit },
            "total": results.len(),
            "results": results,
        });
        if let Some(queries) = queries {
            payload["queries"] = queries;
        }
        if let Some(kind) = args
            .get("kind")
            .and_then(Value::as_str)
            .filter(|k| !k.is_empty())
        {
            payload["filterKind"] = Value::String(kind.to_string());
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

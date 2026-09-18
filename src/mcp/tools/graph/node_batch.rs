//! `codegraph_node` with several symbols in one call.
//!
//! Agents that want three definitions otherwise make three calls or grep a
//! `a|b|c` alternation. Each name runs through the ordinary single-symbol
//! path, so disambiguation, staleness notices, and code inclusion behave
//! exactly as they do for one name; only the results are merged.

use serde_json::{Map, Value, json};

use super::super::context::ToolHandler;
use super::super::format::mcp_output_budget;
use super::super::output::fit_node_payload;
use super::super::schema::ToolResult;
use crate::error::Result;

const MAX_SYMBOLS: usize = 12;

/// Names from `symbols`, with a lone `symbol` counted as the first one.
pub(in crate::mcp::tools) fn batch_symbols(args: &Map<String, Value>) -> Option<Vec<String>> {
    let items = args.get("symbols")?.as_array()?;
    let mut names = Vec::new();
    if let Some(first) = args.get("symbol").and_then(Value::as_str) {
        names.push(first.trim().to_string());
    }
    names.extend(
        items
            .iter()
            .filter_map(Value::as_str)
            .map(|name| name.trim().to_string()),
    );
    names.retain(|name| !name.is_empty());
    names.dedup();
    (names.len() > 1 || (names.len() == 1 && args.get("symbol").is_none())).then_some(names)
}

impl ToolHandler {
    pub(in crate::mcp::tools) fn handle_node_batch(
        &self,
        args: &Map<String, Value>,
        names: Vec<String>,
    ) -> Result<ToolResult> {
        let names: Vec<String> = names.into_iter().take(MAX_SYMBOLS).collect();
        let mut sections = Vec::new();
        let mut matches = Vec::new();
        let mut match_count = 0u64;
        let mut truncated = false;
        let mut notices = Vec::new();

        for name in &names {
            let mut single = args.clone();
            single.remove("symbols");
            single.insert("symbol".into(), Value::String(name.clone()));
            let result = self.handle_node(&single)?;
            sections.push(format!("## `{name}`\n\n{}", result_text(&result)));
            notices.extend(result.meta.into_iter().flat_map(|meta| meta.notices));
            let Some(payload) = result.structured_content.as_ref() else {
                continue;
            };
            if payload.get("kind").and_then(Value::as_str) != Some("node") {
                continue;
            }
            match_count += payload["matchCount"].as_u64().unwrap_or(0);
            truncated |= payload["truncated"].as_bool().unwrap_or(false);
            if let Some(items) = payload["matches"].as_array() {
                matches.extend(items.iter().cloned());
            }
        }

        let mut payload = json!({
            "schemaVersion": 2,
            "kind": "node",
            "matchCount": match_count,
            "matches": matches,
        });
        if truncated {
            payload["truncated"] = Value::Bool(true);
        }
        fit_node_payload(&mut payload, mcp_output_budget());
        let text = self.truncate_output(&sections.join("\n\n"));
        let mut result = self.structured_result(&text, &payload)?;
        for notice in notices {
            result = result.with_notice(notice);
        }
        Ok(result)
    }
}

fn result_text(result: &ToolResult) -> String {
    result
        .content
        .iter()
        .map(|content| content.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

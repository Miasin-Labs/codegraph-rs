//! Calls support for graph MCP tools.

use std::collections::HashSet;

use serde_json::{Map, Value};

use super::super::context::ToolHandler;
use super::super::format::num_or;
use super::super::schema::ToolResult;
use crate::error::Result;
use crate::types::Node;
use crate::utils::clamp;

impl ToolHandler {
    pub(in crate::mcp::tools) fn handle_callers(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolResult> {
        let symbol = match self.validate_string(args.get("symbol"), "symbol") {
            Ok(s) => s,
            Err(r) => return Ok(r),
        };

        let cg = self.get_code_graph(args.get("projectPath").and_then(|v| v.as_str()))?;
        let limit = clamp(num_or(args, "limit", 20.0), 1.0, 100.0) as usize;

        let all_matches = self.find_all_symbols(&cg, &symbol)?;
        if all_matches.nodes.is_empty() {
            return Ok(self.text_result(&format!("Symbol \"{symbol}\" not found in the codebase")));
        }

        // Aggregate callers across all matching symbols
        let mut seen: HashSet<String> = HashSet::new();
        let mut all_callers: Vec<Node> = Vec::new();
        for node in &all_matches.nodes {
            for c in cg.get_callers(&node.id, None)? {
                if seen.insert(c.node.id.clone()) {
                    all_callers.push(c.node);
                }
            }
        }

        if all_callers.is_empty() {
            return Ok(self.text_result(&format!(
                "No callers found for \"{symbol}\"{}",
                all_matches.note
            )));
        }

        all_callers.truncate(limit);
        let formatted = format!(
            "{}{}",
            self.format_node_list(&all_callers, &format!("Callers of {symbol}")),
            all_matches.note
        );
        Ok(self.text_result(&self.truncate_output(&formatted)))
    }

    pub(in crate::mcp::tools) fn handle_callees(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolResult> {
        let symbol = match self.validate_string(args.get("symbol"), "symbol") {
            Ok(s) => s,
            Err(r) => return Ok(r),
        };

        let cg = self.get_code_graph(args.get("projectPath").and_then(|v| v.as_str()))?;
        let limit = clamp(num_or(args, "limit", 20.0), 1.0, 100.0) as usize;

        let all_matches = self.find_all_symbols(&cg, &symbol)?;
        if all_matches.nodes.is_empty() {
            return Ok(self.text_result(&format!("Symbol \"{symbol}\" not found in the codebase")));
        }

        // Aggregate callees across all matching symbols
        let mut seen: HashSet<String> = HashSet::new();
        let mut all_callees: Vec<Node> = Vec::new();
        for node in &all_matches.nodes {
            for c in cg.get_callees(&node.id, None)? {
                if seen.insert(c.node.id.clone()) {
                    all_callees.push(c.node);
                }
            }
        }

        if all_callees.is_empty() {
            return Ok(self.text_result(&format!(
                "No callees found for \"{symbol}\"{}",
                all_matches.note
            )));
        }

        all_callees.truncate(limit);
        let formatted = format!(
            "{}{}",
            self.format_node_list(&all_callees, &format!("Callees of {symbol}")),
            all_matches.note
        );
        Ok(self.text_result(&self.truncate_output(&formatted)))
    }

    // =========================================================================
    // codegraph_impact
}

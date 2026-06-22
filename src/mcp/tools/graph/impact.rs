//! Impact support for graph MCP tools.

use std::collections::HashSet;

use serde_json::{Map, Value};

use super::super::context::ToolHandler;
use super::super::format::{OrderedNodeMap, num_or, ordered_nodes_from_subgraph};
use super::super::schema::ToolResult;
use crate::error::Result;
use crate::utils::clamp;

impl ToolHandler {
    pub(in crate::mcp::tools) fn handle_impact(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolResult> {
        let symbol = match self.validate_string(args.get("symbol"), "symbol") {
            Ok(s) => s,
            Err(r) => return Ok(r),
        };

        let cg = self.get_code_graph(args.get("projectPath").and_then(|v| v.as_str()))?;
        let depth = clamp(num_or(args, "depth", 2.0), 1.0, 10.0) as u32;

        let all_matches = self.find_all_symbols(&cg, &symbol)?;
        if all_matches.nodes.is_empty() {
            return Ok(self.text_result(&format!("Symbol \"{symbol}\" not found in the codebase")));
        }

        // Aggregate impact across all matching symbols
        let mut merged_nodes = OrderedNodeMap::new();
        let mut seen_edges: HashSet<String> = HashSet::new();

        for node in &all_matches.nodes {
            let impact = cg.get_impact_radius(&node.id, Some(depth))?;
            // Subgraph.nodes is a HashMap (the TS Map preserved insertion
            // order) — impose deterministic ordering. See notes/mcp-tools.md.
            let ordered = ordered_nodes_from_subgraph(&impact);
            for n in ordered.values() {
                merged_nodes.insert(n.clone());
            }
            for e in &impact.edges {
                let key = format!("{}->{}:{}", e.source, e.target, e.kind.as_str());
                seen_edges.insert(key);
            }
        }

        let formatted = format!(
            "{}{}",
            self.format_impact(&symbol, &merged_nodes),
            all_matches.note
        );
        Ok(self.text_result(&self.truncate_output(&formatted)))
    }
}

//! `codegraph_impact`, across projects.
//!
//! The in-project blast radius is unchanged; the projects that use this
//! code (atlas path-dependency dependents, or every project pinning a
//! dependency version) add what in them references anything in it, and
//! their own dependents of that up to `depth - 1` — bounded by the project
//! cap, a per-project cap and the call's deadline. A symbol that lives in
//! another graph is measured in that graph first.

use std::collections::HashSet;

use serde_json::{Map, Value};

use super::super::context::ToolHandler;
use super::super::format::{OrderedNodeMap, num_or, ordered_nodes_from_subgraph};
use super::super::schema::ToolResult;
use super::federated::{cross_impact_section, graph_arg};
use crate::codegraph::CodeGraph;
use crate::error::Result;
use crate::federation::{ForeignSymbol, GraphId, GraphSet};
use crate::types::Node;
use crate::utils::clamp;

/// Symbols of a blast radius whose dependents are looked up across
/// projects.
const CROSS_CHANGED: usize = 256;
/// Symbols listed per dependent project.
const PER_PROJECT: usize = 40;

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
        let graph = graph_arg(args);
        let fed = self.federation();

        let all_matches = match graph {
            Some(_) => None,
            None => Some(self.find_all_symbols(&cg, &symbol)?),
        };
        let Some(all_matches) = all_matches.filter(|matches| !matches.nodes.is_empty()) else {
            let foreign = fed
                .as_ref()
                .map(|fed| self.foreign_symbols(fed, &cg, &symbol, graph.as_deref()))
                .unwrap_or_default();
            return match (fed.as_ref(), foreign.is_empty()) {
                (Some(fed), false) => {
                    let text = self.foreign_impact_text(fed, &cg, &symbol, &foreign, depth);
                    Ok(self.text_result(&self.truncate_output(&text)))
                }
                _ => {
                    Ok(self.text_result(&format!("Symbol \"{symbol}\" not found in the codebase")))
                }
            };
        };

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

        let across = match &fed {
            Some(fed) => {
                let changed: Vec<Node> =
                    merged_nodes.values().take(CROSS_CHANGED).cloned().collect();
                let cross = fed.impact_across(
                    &GraphId::project(cg.get_project_root()),
                    &changed,
                    None,
                    depth,
                    PER_PROJECT,
                );
                cross_impact_section(&cross)
            }
            None => String::new(),
        };
        let formatted = format!(
            "{}{across}{}",
            self.format_impact(&symbol, &merged_nodes),
            all_matches.note
        );
        Ok(self.text_result(&self.truncate_output(&formatted)))
    }

    /// The blast radius of a symbol in another graph: inside that graph,
    /// then in every project using it.
    fn foreign_impact_text(
        &self,
        fed: &GraphSet,
        cg: &CodeGraph,
        symbol: &str,
        foreign: &[ForeignSymbol],
        depth: u32,
    ) -> String {
        let mut sections = Vec::new();
        for found in foreign {
            let radius = found
                .graph
                .traverser()
                .get_impact_radius(&found.node.id, depth)
                .unwrap_or_default();
            let ordered = ordered_nodes_from_subgraph(&radius);
            let mut inside = OrderedNodeMap::new();
            inside.insert(found.node.clone());
            for node in ordered.values() {
                inside.insert(node.clone());
            }
            let heading = format!("{symbol} ({})", found.graph.label);
            let changed: Vec<Node> = inside.values().take(CROSS_CHANGED).cloned().collect();
            let cross = fed.impact_across(
                &found.graph.id,
                &changed,
                Some(cg.get_project_root()),
                depth,
                PER_PROJECT,
            );
            sections.push(format!(
                "{}{}",
                self.format_impact(&heading, &inside),
                cross_impact_section(&cross)
            ));
        }
        sections.join("\n\n")
    }
}

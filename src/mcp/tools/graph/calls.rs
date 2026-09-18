//! `codegraph_callers` / `codegraph_callees`, across graphs.
//!
//! In-project answers come first and are unchanged. Callees add the
//! targets of the symbol's external edges (dependency shards, linked
//! projects), followed into their graphs; callers add the callers in
//! every project that uses this code. A symbol that lives in another
//! graph (`serde_json::from_str`, or `graph: "serde_json"`) is answered
//! from that graph, and its callers from every project using it.

use std::collections::HashSet;

use serde_json::{Map, Value};

use super::super::context::ToolHandler;
use super::super::format::num_or;
use super::super::schema::ToolResult;
use super::federated::{
    CALL_KINDS,
    cross_callers_section,
    external_edges_of,
    followed_line,
    foreign_node_line,
    graph_arg,
};
use crate::codegraph::CodeGraph;
use crate::error::Result;
use crate::federation::{ForeignSymbol, GraphId, GraphSet};
use crate::types::{Node, NodeRef};
use crate::utils::clamp;

/// In-project definitions whose dependents are looked up across projects.
const CROSS_TARGETS: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    Callers,
    Callees,
}

impl Direction {
    fn noun(self) -> &'static str {
        match self {
            Self::Callers => "callers",
            Self::Callees => "callees",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Callers => "Callers",
            Self::Callees => "Callees",
        }
    }
}

impl ToolHandler {
    pub(in crate::mcp::tools) fn handle_callers(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolResult> {
        self.handle_calls(args, Direction::Callers)
    }

    pub(in crate::mcp::tools) fn handle_callees(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolResult> {
        self.handle_calls(args, Direction::Callees)
    }

    fn handle_calls(&self, args: &Map<String, Value>, direction: Direction) -> Result<ToolResult> {
        let symbol = match self.validate_string(args.get("symbol"), "symbol") {
            Ok(s) => s,
            Err(r) => return Ok(r),
        };
        let cg = self.get_code_graph(args.get("projectPath").and_then(|v| v.as_str()))?;
        let limit = clamp(num_or(args, "limit", 20.0), 1.0, 100.0) as usize;
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
                (Some(fed), false) => Ok(self.text_result(&self.truncate_output(
                    &foreign_calls_text(fed, &cg, &symbol, &foreign, direction, limit),
                ))),
                _ => Ok(self.text_result(&not_found(&symbol, graph.as_deref()))),
            };
        };

        // Aggregate across all matching symbols.
        let mut seen: HashSet<String> = HashSet::new();
        let mut related: Vec<Node> = Vec::new();
        for node in &all_matches.nodes {
            let refs = match direction {
                Direction::Callers => cg.get_callers(&node.id, None)?,
                Direction::Callees => cg.get_callees(&node.id, None)?,
            };
            for r in refs {
                if seen.insert(r.node.id.clone()) {
                    related.push(r.node);
                }
            }
        }
        related.truncate(limit);

        let federated = match &fed {
            Some(fed) => match direction {
                Direction::Callees => {
                    let ids: Vec<String> = all_matches.nodes.iter().map(|n| n.id.clone()).collect();
                    let mut edges = external_edges_of(&cg, &ids, CALL_KINDS)?;
                    let omitted = edges.len().saturating_sub(limit);
                    edges.truncate(limit);
                    external_callees_section(fed, &edges, omitted)
                }
                Direction::Callers => {
                    let targets: Vec<Node> = all_matches
                        .nodes
                        .iter()
                        .take(CROSS_TARGETS)
                        .cloned()
                        .collect();
                    let cross = fed.callers_across(
                        &GraphId::project(cg.get_project_root()),
                        &targets,
                        None,
                        limit,
                    );
                    cross_callers_section(&cross, "Callers in other projects")
                }
            },
            None => String::new(),
        };

        if related.is_empty() && federated.is_empty() {
            return Ok(self.text_result(&format!(
                "No {} found for \"{symbol}\"{}",
                direction.noun(),
                all_matches.note
            )));
        }
        let own = if related.is_empty() {
            format!("## {} of {symbol}: none in this project", direction.title())
        } else {
            self.format_node_list(&related, &format!("{} of {symbol}", direction.title()))
        };
        let formatted = format!("{own}{federated}{}", all_matches.note);
        Ok(self.text_result(&self.truncate_output(&formatted)))
    }

    // =========================================================================
    // codegraph_impact
}

fn not_found(symbol: &str, graph: Option<&str>) -> String {
    match graph {
        Some(graph) => format!(
            "Symbol \"{symbol}\" not found in \"{graph}\" (a dependency with a built shard or a \
             linked project this project reaches)"
        ),
        None => format!("Symbol \"{symbol}\" not found in the codebase"),
    }
}

/// Callees in other graphs: each external call edge, followed.
fn external_callees_section(
    fed: &GraphSet,
    edges: &[crate::db::ExternalEdge],
    omitted: usize,
) -> String {
    if edges.is_empty() {
        return String::new();
    }
    let followed = fed.follow_all(edges);
    let mut lines = vec![
        String::new(),
        format!("### In other graphs ({})", followed.len() + omitted),
        String::new(),
    ];
    lines.extend(followed.iter().map(followed_line));
    if omitted > 0 {
        lines.push(format!("- … +{omitted} more"));
    }
    lines.join("\n")
}

/// Callers or callees of a symbol that lives in another graph: inside that
/// graph, and (callers) in every project using the graph.
fn foreign_calls_text(
    fed: &GraphSet,
    cg: &CodeGraph,
    symbol: &str,
    foreign: &[ForeignSymbol],
    direction: Direction,
    limit: usize,
) -> String {
    let mut sections = Vec::new();
    for found in foreign {
        let traverser = found.graph.traverser();
        let refs: Vec<NodeRef> = match direction {
            Direction::Callers => traverser.get_callers(&found.node.id, 1),
            Direction::Callees => traverser.get_callees(&found.node.id, 1),
        }
        .unwrap_or_default();
        let omitted = refs.len().saturating_sub(limit);
        let mut lines = vec![format!(
            "## {} of {} ({}) in {} — {}:{} ({} found)",
            direction.title(),
            found.node.qualified_name,
            found.node.kind.as_str(),
            found.graph.label,
            found.node.file_path,
            found.node.start_line,
            refs.len()
        )];
        if !refs.is_empty() {
            lines.push(String::new());
        }
        lines.extend(
            refs.iter()
                .take(limit)
                .map(|r| foreign_node_line(&found.graph.label, &r.node)),
        );
        if omitted > 0 {
            lines.push(format!("- … +{omitted} more"));
        }
        if direction == Direction::Callers {
            let cross = fed.callers_across(
                &found.graph.id,
                std::slice::from_ref(&found.node),
                Some(cg.get_project_root()),
                limit,
            );
            lines.push(cross_callers_section(
                &cross,
                &format!("Callers in projects using {}", found.graph.label),
            ));
        }
        sections.push(lines.join("\n"));
    }
    if sections.is_empty() {
        return format!("Symbol \"{symbol}\" not found");
    }
    sections.join("\n\n")
}

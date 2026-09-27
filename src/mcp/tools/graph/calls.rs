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
use super::super::format::{mcp_output_budget, num_or};
use super::super::output::{
    CallsOutput,
    CrossCallersOutput,
    ExternalRef,
    ForeignCallsOutput,
    SymbolRef,
    fitted,
    foreign_ref,
};
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
use crate::federation::{Followed, ForeignSymbol, GraphId, GraphSet};
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
        let kind = direction.noun();

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
                    let (text, sections) =
                        foreign_calls(fed, &cg, &symbol, &foreign, direction, limit);
                    let mut output = CallsOutput::new(kind);
                    output.not_found = sections.is_empty();
                    output.foreign = sections;
                    self.calls_result(&text, &output)
                }
                _ => self.calls_result(
                    &not_found(&symbol, graph.as_deref()),
                    &CallsOutput::not_found(kind),
                ),
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
        let mut output = CallsOutput::new(kind);
        output.results_omitted = related.len().saturating_sub(limit);
        related.truncate(limit);
        output.results = related.iter().map(SymbolRef::from).collect();
        if all_matches.nodes.len() > 1 {
            output.matches = all_matches.nodes.iter().map(SymbolRef::from).collect();
        }

        let federated = match &fed {
            Some(fed) => match direction {
                Direction::Callees => {
                    let ids: Vec<String> = all_matches.nodes.iter().map(|n| n.id.clone()).collect();
                    let mut edges = external_edges_of(&cg, &ids, CALL_KINDS)?;
                    let omitted = edges.len().saturating_sub(limit);
                    edges.truncate(limit);
                    let followed = fed.follow_all(&edges);
                    output.external = followed.iter().map(ExternalRef::from).collect();
                    output.external_omitted = if followed.is_empty() { 0 } else { omitted };
                    external_callees_section(&followed, omitted)
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
                    output.cross = CrossCallersOutput::from(&cross);
                    cross_callers_section(&cross, "Callers in other projects")
                }
            },
            None => String::new(),
        };

        if related.is_empty() && federated.is_empty() {
            return self.calls_result(
                &format!(
                    "No {} found for \"{symbol}\"{}",
                    direction.noun(),
                    all_matches.note
                ),
                &output,
            );
        }
        let own = if related.is_empty() {
            format!("## {} of {symbol}: none in this project", direction.title())
        } else {
            self.format_node_list(&related, &format!("{} of {symbol}", direction.title()))
        };
        let formatted = format!("{own}{federated}{}", all_matches.note);
        self.calls_result(&formatted, &output)
    }

    /// The human text as-is (the CLI and tests read it) with the payload,
    /// bounded to the MCP budget: foreign sections go first, then other
    /// projects, then the tail of the results.
    fn calls_result(&self, text: &str, output: &CallsOutput) -> Result<ToolResult> {
        let payload = fitted(
            output,
            mcp_output_budget(),
            &["foreign", "otherProjects", "external", "results"],
        );
        self.structured_result(&self.truncate_output(text), &payload)
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
fn external_callees_section(followed: &[Followed], omitted: usize) -> String {
    if followed.is_empty() {
        return String::new();
    }
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
fn foreign_calls(
    fed: &GraphSet,
    cg: &CodeGraph,
    symbol: &str,
    foreign: &[ForeignSymbol],
    direction: Direction,
    limit: usize,
) -> (String, Vec<ForeignCallsOutput>) {
    let mut sections = Vec::new();
    let mut outputs = Vec::new();
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
        let mut output = ForeignCallsOutput {
            symbol: foreign_ref(&found.graph.label, &found.node),
            results: refs
                .iter()
                .take(limit)
                .map(|r| SymbolRef::from(&r.node))
                .collect(),
            results_omitted: omitted,
            cross: CrossCallersOutput::default(),
        };
        if direction == Direction::Callers {
            let cross = fed.callers_across(
                &found.graph.id,
                std::slice::from_ref(&found.node),
                Some(cg.get_project_root()),
                limit,
            );
            output.cross = CrossCallersOutput::from(&cross);
            lines.push(cross_callers_section(
                &cross,
                &format!("Callers in projects using {}", found.graph.label),
            ));
        }
        sections.push(lines.join("\n"));
        outputs.push(output);
    }
    if sections.is_empty() {
        return (format!("Symbol \"{symbol}\" not found"), outputs);
    }
    (sections.join("\n\n"), outputs)
}

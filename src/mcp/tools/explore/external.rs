//! What the answer's code calls in other graphs: dependency shards and
//! linked projects, through the external edges of the symbols in the files
//! explore is about to show. The most-called targets come back as compact
//! rows — the graph, the target's qualified name and place there (followed
//! into its graph, so a rebuilt graph still answers and a missing one says
//! so), its signature, and the answer's symbol that calls it — never the
//! target's source (`codegraph_node` reads that from the dependency).

use std::collections::HashMap;

use serde::Serialize;

use super::types::RankedExploreFiles;
use crate::codegraph::CodeGraph;
use crate::db::ExternalEdge;
use crate::mcp::tools::graph::federated::{CALL_KINDS, availability_note};
use crate::mcp::tools::output::compact_signature;
use crate::types::Node;

/// Rows at most.
const MAX_ROWS: usize = 8;
/// Symbols whose edges are read at most.
const MAX_SOURCES: usize = 400;
/// Longest signature a row carries.
const MAX_SIGNATURE: usize = 120;

fn is_one(value: &usize) -> bool {
    *value <= 1
}

/// A symbol of another graph the answer's code calls.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools::explore) struct ExploreExternal {
    /// `serde_json@1.0.150`, or a linked project's name.
    pub graph: String,
    /// Its qualified name in that graph (`Connection::prepare`).
    pub symbol: String,
    pub kind: &'static str,
    /// Relative to that graph's root.
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// The answer's symbol that calls it first.
    pub from: String,
    /// Call sites in the answer's symbols that reach it (when more than one).
    #[serde(skip_serializing_if = "is_one")]
    pub calls: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

/// The rows, and the characters to hold back from source for them.
#[derive(Default)]
pub(in crate::mcp::tools::explore) struct ExternalPlan {
    pub rows: Vec<ExploreExternal>,
    pub reserve: usize,
}

impl ExternalPlan {
    /// Markdown for the human (CLI) text.
    pub fn append_markdown(&self, lines: &mut Vec<String>) {
        if self.rows.is_empty() {
            return;
        }
        lines.push(
            "### Into other graphs — what this code calls in dependencies and linked projects"
                .to_string(),
        );
        lines.push(String::new());
        for row in &self.rows {
            let location = match row.line {
                Some(line) => format!("{}:{line}", row.file),
                None => row.file.clone(),
            };
            let calls = if row.calls > 1 {
                format!(", {} calls", row.calls)
            } else {
                String::new()
            };
            let note = row
                .unavailable
                .as_deref()
                .map(|note| format!(" ({note})"))
                .unwrap_or_default();
            lines.push(format!(
                "- {} ({}) - {} {location} ← {}{calls}{note}",
                row.symbol, row.kind, row.graph, row.from
            ));
        }
        lines.push(String::new());
    }
}

/// The most-called targets in other graphs of the symbols in explore's
/// first `max_files` ranked files. Empty when the reader is off, the
/// project resolved nothing externally, or a read fails.
pub(in crate::mcp::tools::explore) fn plan_external(
    fed: Option<&crate::federation::GraphSet>,
    cg: &CodeGraph,
    ranked: &RankedExploreFiles,
    max_files: usize,
) -> ExternalPlan {
    let Some(fed) = fed else {
        return ExternalPlan::default();
    };
    let nodes: HashMap<&str, &Node> = ranked
        .sorted_files
        .iter()
        .take(max_files)
        .filter_map(|file| ranked.file_groups.get(file))
        .flat_map(|group| group.nodes.iter())
        .take(MAX_SOURCES)
        .map(|node| (node.id.as_str(), node))
        .collect();
    if nodes.is_empty() {
        return ExternalPlan::default();
    }
    let ids: Vec<String> = nodes.keys().map(|id| id.to_string()).collect();
    let Ok(edges) = cg.get_external_edges(&ids) else {
        return ExternalPlan::default();
    };
    // Targets by call count, then by where they are first called.
    let mut targets: Vec<(ExternalEdge, usize)> = Vec::new();
    let mut index: HashMap<(String, String), usize> = HashMap::new();
    for edge in edges
        .into_iter()
        .filter(|edge| CALL_KINDS.contains(&edge.kind))
    {
        let key = (edge.target_graph_key.clone(), edge.target_node_id.clone());
        match index.get(&key) {
            Some(&at) => targets[at].1 += 1,
            None => {
                index.insert(key, targets.len());
                targets.push((edge, 1));
            }
        }
    }
    targets.sort_by_key(|target| std::cmp::Reverse(target.1));
    targets.truncate(MAX_ROWS);
    let rows: Vec<ExploreExternal> = targets
        .into_iter()
        .map(|(edge, calls)| {
            let followed = fed.follow(&edge);
            let from = nodes
                .get(edge.source.as_str())
                .map_or_else(String::new, |node| node.name.clone());
            ExploreExternal {
                graph: followed.label.clone(),
                symbol: edge.target_qualified_name.clone(),
                kind: edge.target_kind.as_str(),
                file: followed.file().to_string(),
                line: followed.line(),
                signature: followed
                    .target
                    .node()
                    .and_then(|node| node.signature.as_deref())
                    .and_then(|signature| compact_signature(signature, MAX_SIGNATURE)),
                from,
                calls,
                unavailable: availability_note(&followed),
            }
        })
        .collect();
    let reserve = rows
        .iter()
        .map(|row| serde_json::to_string(row).map_or(0, |text| text.len() + 1))
        .sum::<usize>()
        + 16;
    ExternalPlan { rows, reserve }
}

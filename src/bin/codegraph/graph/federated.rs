//! The graph commands across graphs (federation phase 3): the same
//! cross-graph answers the MCP tools give — external edges followed into
//! dependency shards and linked projects, callers and impact in the other
//! projects that use this code, and symbols that live in another graph —
//! as text (the shared renderers) and as JSON.

use std::path::Path;

use codegraph::Node;
use codegraph::federation::render::{cross_callers_section, cross_impact_section, followed_line};
use codegraph::federation::{
    CrossCallers,
    CrossImpact,
    FederationOptions,
    Followed,
    ForeignSymbol,
    GraphId,
    GraphSet,
    SkippedProject,
    federation_enabled,
};
use serde_json::{Value, json};

/// Definitions one foreign name may resolve to.
const FOREIGN_MATCHES: usize = 8;

/// The cross-graph reader, unless `CODEGRAPH_FEDERATION=0`.
pub(crate) fn reader() -> Option<GraphSet> {
    federation_enabled().then(|| GraphSet::new(FederationOptions::from_env()))
}

/// The graph of the project at `root`.
pub(crate) fn project_graph(root: &Path) -> GraphId {
    GraphId::project(root)
}

/// Symbols `symbol` names in the graphs the project at `root` reaches.
pub(crate) fn foreign_symbols(fed: &GraphSet, root: &Path, symbol: &str) -> Vec<ForeignSymbol> {
    fed.resolve_symbol(root, symbol, None, FOREIGN_MATCHES)
}

pub(crate) fn node_json(node: &Node) -> Value {
    json!({
        "name": node.name,
        "kind": node.kind.as_str(),
        "filePath": node.file_path,
        "startLine": node.start_line,
    })
}

/// A followed external edge: the phase-2 fields plus the target graph's
/// label and, when it could not be read, why.
pub(crate) fn followed_json(followed: &Followed) -> Value {
    let edge = &followed.edge;
    let mut row = json!({
        "graph": edge.target_graph_key,
        "graphKind": edge.target_graph_kind.as_str(),
        "graphLabel": followed.label,
        "name": edge.target_name,
        "qualifiedName": edge.target_qualified_name,
        "kind": edge.target_kind.as_str(),
        "filePath": followed.file(),
        "startLine": followed.line(),
    });
    if let Some(note) = codegraph::federation::render::availability_note(followed) {
        row["unavailable"] = Value::String(note);
    }
    row
}

pub(crate) fn followed_lines(followed: &[Followed], indent: &str) -> Vec<String> {
    followed
        .iter()
        .map(|item| format!("{indent}{}", followed_line(item)))
        .collect()
}

fn skipped_json(skipped: &[SkippedProject]) -> Value {
    Value::Array(
        skipped
            .iter()
            .map(|entry| {
                json!({
                    "project": entry.project.name,
                    "root": entry.project.root,
                    "reason": entry.reason.as_str(),
                })
            })
            .collect(),
    )
}

/// `otherProjects` / `skippedProjects` of a cross-project callers answer.
pub(crate) fn cross_callers_json(cross: &CrossCallers) -> (Value, Value) {
    let groups = cross
        .groups
        .iter()
        .map(|group| {
            json!({
                "project": group.project.name,
                "root": group.project.root,
                "callers": group.callers.iter().map(|caller| node_json(&caller.node)).collect::<Vec<_>>(),
                "omitted": group.omitted,
                "partial": group.partial,
            })
        })
        .collect();
    (Value::Array(groups), skipped_json(&cross.skipped))
}

/// `otherProjects` / `skippedProjects` of a cross-project impact answer.
pub(crate) fn cross_impact_json(cross: &CrossImpact) -> (Value, Value) {
    let groups = cross
        .groups
        .iter()
        .map(|group| {
            json!({
                "project": group.project.name,
                "root": group.project.root,
                "entries": group.entries.iter().map(|entry| node_json(&entry.node)).collect::<Vec<_>>(),
                "affected": group.affected.iter().map(node_json).collect::<Vec<_>>(),
                "omitted": group.omitted,
                "partial": group.partial,
            })
        })
        .collect();
    (Value::Array(groups), skipped_json(&cross.skipped))
}

/// Print a cross-project section (text mode), when there is one.
pub(crate) fn print_section(section: &str) {
    if !section.is_empty() {
        println!("{section}\n");
    }
}

pub(crate) fn callers_text(cross: &CrossCallers, heading: &str) -> String {
    cross_callers_section(cross, heading)
}

pub(crate) fn impact_text(cross: &CrossImpact) -> String {
    cross_impact_section(cross)
}

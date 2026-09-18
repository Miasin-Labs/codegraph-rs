//! Text for cross-graph answers, shared by the MCP tools and the CLI: the
//! `- name (kind) - place` lines the graph tools already print, with the
//! graph (`serde_json@1.0.150`, `linkscope`) or project each row is in, and
//! a closing line naming what could not be read and why.

use std::collections::HashSet;

use super::{CrossCallers, CrossImpact, Followed, SkippedProject, Target};
use crate::types::Node;

/// Projects a skip line names before "+N more".
const SKIPPED_NAMED: usize = 6;

/// Why a followed target cannot be shown, as a trailing note.
pub fn availability_note(followed: &Followed) -> Option<String> {
    match &followed.target {
        Target::Found(_) | Target::Moved(_) | Target::Recorded => None,
        Target::Gone => Some(format!("no longer in {}", followed.label)),
        Target::Unavailable(reason) => Some(format!(
            "target not available: {}",
            reason.describe(followed.edge.target_graph_kind)
        )),
    }
}

/// `- Connection::open (method) - rusqlite@0.32.1 src/lib.rs:450`.
pub fn followed_line(followed: &Followed) -> String {
    let location = match followed.line() {
        Some(line) if line > 0 => format!("{}:{line}", followed.file()),
        _ => followed.file().to_string(),
    };
    let note = availability_note(followed)
        .map(|note| format!(" ({note})"))
        .unwrap_or_default();
    format!(
        "- {} ({}) - {} {location}{note}",
        followed.edge.target_qualified_name,
        followed.edge.target_kind.as_str(),
        followed.label
    )
}

/// `- name (kind) - label file:line` for a node of another graph.
pub fn foreign_node_line(label: &str, node: &Node) -> String {
    format!(
        "- {} ({}) - {label} {}:{}",
        node.name,
        node.kind.as_str(),
        node.file_path,
        node.start_line
    )
}

/// `- name (kind) - file:line` for a node of a project.
fn project_node_line(node: &Node) -> String {
    if node.start_line > 0 {
        format!(
            "- {} ({}) - {}:{}",
            node.name,
            node.kind.as_str(),
            node.file_path,
            node.start_line
        )
    } else {
        format!(
            "- {} ({}) - {}",
            node.name,
            node.kind.as_str(),
            node.file_path
        )
    }
}

/// `Not read: peony (external resolution has not run…), jfc (no index)`.
fn skipped_line(skipped: &[SkippedProject]) -> Option<String> {
    if skipped.is_empty() {
        return None;
    }
    let named: Vec<String> = skipped
        .iter()
        .take(SKIPPED_NAMED)
        .map(|entry| format!("{} ({})", entry.project.name, entry.reason.describe()))
        .collect();
    let more = skipped.len().saturating_sub(SKIPPED_NAMED);
    Some(format!(
        "Not read: {}{}",
        named.join(", "),
        if more > 0 {
            format!(", +{more} more")
        } else {
            String::new()
        }
    ))
}

/// The callers of the same code in the projects that use it, by project.
pub fn cross_callers_section(cross: &CrossCallers, heading: &str) -> String {
    if cross.is_empty() {
        return String::new();
    }
    let callers: usize = cross
        .groups
        .iter()
        .map(|group| group.callers.len() + group.omitted)
        .sum();
    let read = cross.groups.len() + cross.without_callers.len();
    // Which item each caller reaches, only when there are several.
    let targets: HashSet<&str> = cross
        .groups
        .iter()
        .flat_map(|group| group.callers.iter().map(|caller| caller.target.as_str()))
        .collect();
    let mut lines = vec![
        String::new(),
        format!(
            "### {heading} — {callers} in {} of {read} project{} read",
            cross.groups.len(),
            if read == 1 { "" } else { "s" }
        ),
    ];
    for group in &cross.groups {
        lines.push(String::new());
        lines.push(format!(
            "#### {} ({}){}",
            group.project.name,
            group.callers.len() + group.omitted,
            if group.partial {
                " — its external pass is incomplete"
            } else {
                ""
            }
        ));
        for caller in &group.callers {
            let target = if targets.len() > 1 {
                format!(" → {}", caller.target)
            } else {
                String::new()
            };
            lines.push(format!("{}{target}", project_node_line(&caller.node)));
        }
        if group.omitted > 0 {
            lines.push(format!("- … +{} more", group.omitted));
        }
    }
    if !cross.without_callers.is_empty() {
        lines.push(String::new());
        lines.push(format!(
            "No calls from: {}",
            cross
                .without_callers
                .iter()
                .map(|project| project.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(line) = skipped_line(&cross.skipped) {
        lines.push(String::new());
        lines.push(line);
    }
    lines.join("\n")
}

/// The part of a blast radius in the projects that use the code.
pub fn cross_impact_section(cross: &CrossImpact) -> String {
    if cross.is_empty() {
        return String::new();
    }
    let mut lines = vec![
        String::new(),
        format!(
            "### Across projects — {} symbols in {} project{}",
            cross.total(),
            cross.groups.len(),
            if cross.groups.len() == 1 { "" } else { "s" }
        ),
    ];
    for group in &cross.groups {
        lines.push(String::new());
        lines.push(format!(
            "#### {} ({} calling in, {} depending on them{}){}",
            group.project.name,
            group.entries.len(),
            group.affected.len(),
            if group.omitted > 0 {
                format!(", +{} more", group.omitted)
            } else {
                String::new()
            },
            if group.partial {
                " — its external pass is incomplete"
            } else {
                ""
            }
        ));
        let mut by_file: Vec<(&str, Vec<String>)> = Vec::new();
        let entries = group.entries.iter().map(|entry| &entry.node);
        for node in entries.chain(group.affected.iter()) {
            let label = format!("{}:{}", node.name, node.start_line);
            match by_file.iter_mut().find(|(file, _)| *file == node.file_path) {
                Some((_, names)) => names.push(label),
                None => by_file.push((node.file_path.as_str(), vec![label])),
            }
        }
        for (file, names) in by_file {
            lines.push(format!("**{file}:** {}", names.join(", ")));
        }
    }
    if !cross.unaffected.is_empty() {
        lines.push(String::new());
        lines.push(format!(
            "Not affected: {}",
            cross
                .unaffected
                .iter()
                .map(|project| project.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(line) = skipped_line(&cross.skipped) {
        lines.push(String::new());
        lines.push(line);
    }
    lines.join("\n")
}

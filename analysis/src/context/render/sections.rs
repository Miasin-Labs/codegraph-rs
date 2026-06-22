use std::collections::HashMap;

use super::labels::{kind_label, line_label, line_suffix, signature_for, symbol_label};
use crate::graph::CodeGraph;
use crate::nodes::{NodeData, NodeId};

pub(super) fn push_entry_points(out: &mut String, graph: &CodeGraph, entry_points: &[NodeId]) {
    if entry_points.is_empty() {
        return;
    }
    out.push_str("### Entry Points\n\n");
    for id in entry_points {
        let Some(node) = graph.get_node(id) else {
            continue;
        };
        out.push_str(&format!(
            "- **{}** ({}) — {}{}\n",
            symbol_label(node),
            kind_label(node.kind),
            node.file_path.display(),
            line_suffix(node),
        ));
        let sig = signature_for(node);
        if !sig.is_empty() {
            out.push_str(&format!("  `{sig}`\n"));
        }
    }
    out.push('\n');
}

pub(super) fn push_related_symbols(out: &mut String, graph: &CodeGraph, related: &[NodeId]) {
    if related.is_empty() {
        return;
    }
    out.push_str("### Related Symbols\n\n");
    let mut by_file: HashMap<std::path::PathBuf, Vec<&NodeData>> = HashMap::new();
    for id in related {
        if let Some(node) = graph.get_node(id) {
            by_file
                .entry(node.file_path.clone())
                .or_default()
                .push(node);
        }
    }
    let mut files: Vec<_> = by_file.into_iter().collect();
    files.sort_by_key(|(p, _)| p.clone());
    for (file, symbols) in files {
        let inline: Vec<String> = symbols.iter().map(|n| line_label(n)).collect();
        out.push_str(&format!("- {}: {}\n", file.display(), inline.join(", ")));
    }
    out.push('\n');
}

pub(super) fn push_code_blocks(
    out: &mut String,
    graph: &CodeGraph,
    code_blocks: &[(NodeId, String)],
) {
    if code_blocks.is_empty() {
        return;
    }
    out.push_str("### Code\n\n");
    for (id, body) in code_blocks {
        let Some(node) = graph.get_node(id) else {
            continue;
        };
        out.push_str(&format!(
            "#### {} ({}:{})\n\n",
            symbol_label(node),
            node.file_path.display(),
            node.span.start_line,
        ));
        out.push_str("```rust\n");
        out.push_str(body);
        if !body.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("```\n\n");
    }
}

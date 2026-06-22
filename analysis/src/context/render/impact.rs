use std::collections::HashMap;

use super::labels::line_label;
use crate::graph::CodeGraph;
use crate::nodes::{NodeData, NodeId};

/// Render an impact result grouped by file. Each file block lists the
/// affected symbols inline (`name:line, name:line, ...`).
pub fn render_impact(
    graph: &CodeGraph,
    symbol: &str,
    nodes: &[NodeId],
    note: Option<&str>,
) -> String {
    let mut by_file: HashMap<std::path::PathBuf, Vec<&NodeData>> = HashMap::new();
    let mut total = 0usize;
    for id in nodes {
        if let Some(node) = graph.get_node(id) {
            by_file
                .entry(node.file_path.clone())
                .or_default()
                .push(node);
            total += 1;
        }
    }

    let mut out = String::new();
    out.push_str(&format!(
        "## Impact: `{symbol}` affects {total} symbols across {} files\n\n",
        by_file.len()
    ));
    let mut files: Vec<_> = by_file.into_iter().collect();
    files.sort_by_key(|b| std::cmp::Reverse(b.1.len()));
    for (file, mut symbols) in files {
        out.push_str(&format!("**{}:**\n", file.display()));
        symbols.sort_by_key(|n| n.span.start_line);
        let inline: Vec<String> = symbols.iter().map(|n| line_label(n)).collect();
        out.push_str(&inline.join(", "));
        out.push_str("\n\n");
    }
    if let Some(n) = note {
        out.push_str(&format!("\n> **Note:** {n}\n"));
    }
    out
}

//! Text renderers for search, impact, and node detail tools.

use std::collections::HashMap;

use super::super::context::ToolHandler;
use super::{OrderedNodeMap, number_source_lines};
use crate::codegraph::CodeGraph;
use crate::error::Result;
use crate::types::{Node, NodeKind, SearchResult};

impl ToolHandler {
    pub(in crate::mcp::tools) fn format_search_results(&self, results: &[SearchResult]) -> String {
        let mut lines: Vec<String> = vec![
            format!("## Search Results ({} found)", results.len()),
            String::new(),
        ];

        for result in results {
            let node = &result.node;
            let location = if node.start_line > 0 {
                format!(":{}", node.start_line)
            } else {
                String::new()
            };
            // Compact format: one line per result with key info
            lines.push(format!("### {} ({})", node.name, node.kind.as_str()));
            lines.push(format!("{}{}", node.file_path, location));
            if let Some(sig) = &node.signature {
                if !sig.is_empty() {
                    lines.push(format!("`{sig}`"));
                }
            }
            lines.push(String::new());
        }

        lines.join("\n")
    }

    pub(in crate::mcp::tools) fn format_node_list(&self, nodes: &[Node], title: &str) -> String {
        let mut lines: Vec<String> = vec![
            format!("## {} ({} found)", title, nodes.len()),
            String::new(),
        ];

        for node in nodes {
            let location = if node.start_line > 0 {
                format!(":{}", node.start_line)
            } else {
                String::new()
            };
            // Compact: just name, kind, location
            lines.push(format!(
                "- {} ({}) - {}{}",
                node.name,
                node.kind.as_str(),
                node.file_path,
                location
            ));
        }

        lines.join("\n")
    }

    pub(in crate::mcp::tools) fn format_impact(
        &self,
        symbol: &str,
        nodes: &OrderedNodeMap,
    ) -> String {
        let node_count = nodes.len();

        // Compact format: just list affected symbols grouped by file
        let mut lines: Vec<String> = vec![
            format!("## Impact: \"{symbol}\" affects {node_count} symbols"),
            String::new(),
        ];

        // Group by file
        let mut file_order: Vec<String> = Vec::new();
        let mut by_file: HashMap<String, Vec<&Node>> = HashMap::new();
        for node in nodes.values() {
            if !by_file.contains_key(&node.file_path) {
                file_order.push(node.file_path.clone());
            }
            by_file
                .entry(node.file_path.clone())
                .or_default()
                .push(node);
        }

        for file in &file_order {
            lines.push(format!("**{file}:**"));
            let node_list = by_file[file]
                .iter()
                .map(|n| format!("{}:{}", n.name, n.start_line))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(node_list);
            lines.push(String::new());
        }

        lines.join("\n")
    }

    /// Build a compact structural outline of a container symbol from its
    /// indexed children. Returns "" when the container has no indexed
    /// children.
    pub(in crate::mcp::tools) fn build_container_outline(
        &self,
        cg: &CodeGraph,
        node: &Node,
    ) -> Result<String> {
        let mut children: Vec<Node> = cg
            .get_children(&node.id)?
            .into_iter()
            .filter(|c| c.kind != NodeKind::Import && c.kind != NodeKind::Export)
            .collect();
        children.sort_by_key(|c| c.start_line);
        if children.is_empty() {
            return Ok(String::new());
        }

        let mut lines: Vec<String> =
            vec![format!("**Members ({}):**", children.len()), String::new()];
        for c in &children {
            let loc = if c.start_line > 0 {
                format!(":{}", c.start_line)
            } else {
                String::new()
            };
            let sig = c
                .signature
                .as_ref()
                .filter(|s| !s.is_empty())
                .map(|s| format!(" — `{s}`"))
                .unwrap_or_default();
            lines.push(format!("- {} ({}){}{}", c.name, c.kind.as_str(), loc, sig));
        }
        Ok(lines.join("\n"))
    }

    pub(in crate::mcp::tools) fn format_node_details(
        &self,
        node: &Node,
        code: Option<&str>,
        outline: Option<&str>,
    ) -> String {
        let location = if node.start_line > 0 {
            format!(":{}", node.start_line)
        } else {
            String::new()
        };
        let mut lines: Vec<String> = vec![
            format!("## {} ({})", node.name, node.kind.as_str()),
            String::new(),
            format!("**Location:** {}{}", node.file_path, location),
        ];

        if let Some(sig) = &node.signature {
            if !sig.is_empty() {
                lines.push(format!("**Signature:** `{sig}`"));
            }
        }

        // Only include docstring if it's short and useful
        if let Some(doc) = &node.docstring {
            if !doc.is_empty() && doc.chars().count() < 200 {
                lines.push(String::new());
                lines.push(doc.clone());
            }
        }

        if let Some(outline) = outline {
            lines.push(String::new());
            lines.push(outline.to_string());
            lines.push(String::new());
            lines.push(format!(
                "> Structural outline only. Read `{}` or call codegraph_node on a specific member for its body.",
                node.file_path
            ));
        } else if let Some(code) = code {
            // Line-numbered (cat -n style) so the agent can cite/edit exact
            // lines without re-Reading the file for them.
            let numbered = if node.start_line > 0 {
                number_source_lines(code, node.start_line as usize)
            } else {
                code.to_string()
            };
            lines.push(String::new());
            lines.push(format!("```{}", node.language.as_str()));
            lines.push(numbered);
            lines.push("```".to_string());
        }

        lines.join("\n")
    }
}

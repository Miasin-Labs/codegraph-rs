//! Context Formatter
//!
//! Formats TaskContext as markdown or JSON for consumption by Claude.
//!
//! Ported from `src/context/formatter.ts`. Output is byte-compatible with the
//! TS implementation for a given node ordering; because `Subgraph.nodes` is a
//! `HashMap` (the TS `Map` insertion order is not representable), non-entry-point
//! nodes are ordered deterministically by (filePath, startLine, name, id) — see
//! `notes/context.md`.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::extraction::generated_detection::is_generated_file;
use crate::types::{
    Edge,
    EdgeKind,
    Language,
    Node,
    NodeKind,
    Subgraph,
    TaskContext,
    TaskContextStats,
    Visibility,
};

/// Order the subgraph's nodes deterministically: entry points first (in entry
/// point order), then the remaining nodes sorted by (filePath, startLine,
/// name, id).
///
/// The TS implementation relies on `Map` insertion order (entry points are
/// inserted first by the builder, traversal results after); `HashMap` has no
/// insertion order, so this is the closest deterministic equivalent.
fn ordered_subgraph_nodes(context: &TaskContext) -> Vec<&Node> {
    let mut out: Vec<&Node> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for ep in &context.entry_points {
        if let Some(n) = context.subgraph.nodes.get(&ep.id) {
            if seen.insert(n.id.as_str()) {
                out.push(n);
            }
        }
    }
    let mut rest: Vec<&Node> = context
        .subgraph
        .nodes
        .values()
        .filter(|n| !seen.contains(n.id.as_str()))
        .collect();
    rest.sort_by(|a, b| {
        a.file_path
            .cmp(&b.file_path)
            .then(a.start_line.cmp(&b.start_line))
            .then(a.name.cmp(&b.name))
            .then(a.id.cmp(&b.id))
    });
    out.extend(rest);
    out
}

/// Format context as markdown
///
/// Creates a compact markdown document optimized for Claude with minimal context usage:
/// - Brief summary
/// - Entry points with locations
/// - Code blocks only for key symbols
pub fn format_context_as_markdown(context: &TaskContext) -> String {
    let mut lines: Vec<String> = Vec::new();

    // Header with query
    lines.push("## Code Context\n".to_string());
    lines.push(format!("**Query:** {}\n", context.query));

    // Entry points - compact format. Re-sort so generated files (.pb.go,
    // .pulsar.go, mocks, …) rank LAST — a flow query should lead with the
    // hand-written implementation, not protobuf scaffolding.
    let mut ordered_entries: Vec<&Node> = context.entry_points.iter().collect();
    ordered_entries.sort_by_key(|n| {
        if is_generated_file(&n.file_path) {
            1
        } else {
            0
        }
    });
    if !ordered_entries.is_empty() {
        lines.push("### Entry Points\n".to_string());
        for node in &ordered_entries {
            let location = if node.start_line != 0 {
                format!(":{}", node.start_line)
            } else {
                String::new()
            };
            lines.push(format!(
                "- **{}** ({}) - {}{}",
                node.name, node.kind, node.file_path, location
            ));
            if let Some(sig) = node.signature.as_deref() {
                if !sig.is_empty() {
                    lines.push(format!("  `{sig}`"));
                }
            }
        }
        lines.push(String::new());
    }

    // Related symbols - compact list (skip verbose structure tree). Drop nodes
    // in generated source files (`.pb.go` / `.pulsar.go` / mocks / …) — agents
    // chasing a flow never want to land on protobuf scaffolding (cosmos-Q3 used
    // to list `gov.pulsar.go::GetExpeditedThreshold` and `1.pulsar.go::Get` in
    // Related Symbols, pure noise that displaced real-flow entries).
    let entry_ids: HashSet<&str> = context.entry_points.iter().map(|e| e.id.as_str()).collect();
    let other_symbols: Vec<&Node> = ordered_subgraph_nodes(context)
        .into_iter()
        .filter(|n| !entry_ids.contains(n.id.as_str()))
        .filter(|n| !is_generated_file(&n.file_path))
        .take(10) // Limit to 10 related symbols
        .collect();

    if !other_symbols.is_empty() {
        lines.push("### Related Symbols\n".to_string());
        // Group by file, preserving first-seen file order (mirrors the TS Map).
        let mut file_order: Vec<&str> = Vec::new();
        let mut by_file: HashMap<&str, Vec<&Node>> = HashMap::new();
        for node in &other_symbols {
            let entry = by_file.entry(node.file_path.as_str()).or_insert_with(|| {
                file_order.push(node.file_path.as_str());
                Vec::new()
            });
            entry.push(node);
        }

        for file in file_order {
            let nodes = &by_file[file];
            let node_list = nodes
                .iter()
                .map(|n| format!("{}:{}", n.name, n.start_line))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("- {file}: {node_list}"));
        }
        lines.push(String::new());
    }

    // Code blocks - only for key entry points. Re-sort so non-generated blocks
    // show first (consistent with Entry Points reordering above).
    if !context.code_blocks.is_empty() {
        let mut ordered_blocks: Vec<&crate::types::CodeBlock> =
            context.code_blocks.iter().collect();
        ordered_blocks.sort_by_key(|b| {
            if is_generated_file(&b.file_path) {
                1
            } else {
                0
            }
        });
        lines.push("### Code\n".to_string());
        for block in ordered_blocks {
            let node_name = block
                .node
                .as_ref()
                .map(|n| n.name.as_str())
                .unwrap_or("Unknown");
            lines.push(format!(
                "#### {} ({}:{})\n",
                node_name, block.file_path, block.start_line
            ));
            lines.push(format!("```{}", block.language));
            lines.push(block.content.clone());
            lines.push("```\n".to_string());
        }
    }

    lines.join("\n")
}

// =============================================================================
// JSON serialization shapes (field order matches the TS serializeNode /
// serializeEdge object-literal order; undefined fields are omitted like
// JSON.stringify does)
// =============================================================================

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonNode<'a> {
    id: &'a str,
    kind: NodeKind,
    name: &'a str,
    qualified_name: &'a str,
    file_path: &'a str,
    language: Language,
    start_line: u32,
    end_line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    signature: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    docstring: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    visibility: Option<Visibility>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_exported: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_async: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_static: Option<bool>,
}

/// Serialize a node for JSON output
fn serialize_node(node: &Node) -> JsonNode<'_> {
    JsonNode {
        id: &node.id,
        kind: node.kind,
        name: &node.name,
        qualified_name: &node.qualified_name,
        file_path: &node.file_path,
        language: node.language,
        start_line: node.start_line,
        end_line: node.end_line,
        signature: node.signature.as_deref(),
        docstring: node.docstring.as_deref(),
        visibility: node.visibility,
        is_exported: node.is_exported,
        is_async: node.is_async,
        is_static: node.is_static,
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonEdge<'a> {
    source: &'a str,
    target: &'a str,
    kind: EdgeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    column: Option<u32>,
}

/// Serialize an edge for JSON output
fn serialize_edge(edge: &Edge) -> JsonEdge<'_> {
    JsonEdge {
        source: &edge.source,
        target: &edge.target,
        kind: edge.kind,
        line: edge.line,
        column: edge.column,
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonCodeBlock<'a> {
    file_path: &'a str,
    start_line: u32,
    end_line: u32,
    language: Language,
    content: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    node_name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    node_kind: Option<NodeKind>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonTaskContext<'a> {
    query: &'a str,
    summary: &'a str,
    entry_points: Vec<JsonNode<'a>>,
    nodes: Vec<JsonNode<'a>>,
    edges: Vec<JsonEdge<'a>>,
    code_blocks: Vec<JsonCodeBlock<'a>>,
    related_files: &'a [String],
    stats: &'a TaskContextStats,
}

/// Format context as JSON
///
/// Returns a structured JSON representation suitable for programmatic use.
pub fn format_context_as_json(context: &TaskContext) -> String {
    // Convert Map to array for JSON serialization
    let serializable = JsonTaskContext {
        query: &context.query,
        summary: &context.summary,
        entry_points: context.entry_points.iter().map(serialize_node).collect(),
        nodes: ordered_subgraph_nodes(context)
            .into_iter()
            .map(serialize_node)
            .collect(),
        edges: context.subgraph.edges.iter().map(serialize_edge).collect(),
        code_blocks: context
            .code_blocks
            .iter()
            .map(|block| JsonCodeBlock {
                file_path: &block.file_path,
                start_line: block.start_line,
                end_line: block.end_line,
                language: block.language,
                content: &block.content,
                node_name: block.node.as_ref().map(|n| n.name.as_str()),
                node_kind: block.node.as_ref().map(|n| n.kind),
            })
            .collect(),
        related_files: &context.related_files,
        stats: &context.stats,
    };

    serde_json::to_string_pretty(&serializable).unwrap_or_else(|_| String::from("{}"))
}

/// Format a subgraph as an ASCII tree structure
pub fn format_subgraph_tree(subgraph: &Subgraph, entry_points: &[Node]) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut printed: HashSet<String> = HashSet::new();

    // Build adjacency list for outgoing edges
    let mut outgoing: HashMap<&str, Vec<&Edge>> = HashMap::new();
    for edge in &subgraph.edges {
        outgoing.entry(edge.source.as_str()).or_default().push(edge);
    }

    // Print each entry point as a tree root
    for entry in entry_points {
        format_node_tree(entry, subgraph, &outgoing, &mut printed, &mut lines, 0, "");
        lines.push(String::new()); // Blank line between trees
    }

    // Print any remaining nodes not reached from entry points.
    // (Deterministic order: the TS version iterates Map insertion order.)
    let mut remaining: Vec<&Node> = subgraph
        .nodes
        .values()
        .filter(|n| !printed.contains(&n.id))
        .collect();
    remaining.sort_by(|a, b| {
        a.file_path
            .cmp(&b.file_path)
            .then(a.start_line.cmp(&b.start_line))
            .then(a.name.cmp(&b.name))
            .then(a.id.cmp(&b.id))
    });

    if !remaining.is_empty() && remaining.len() <= 10 {
        lines.push("Other relevant symbols:".to_string());
        for node in remaining {
            let location = if node.start_line != 0 {
                format!(":{}", node.start_line)
            } else {
                String::new()
            };
            lines.push(format!(
                "  {}: {} ({}{})",
                node.kind, node.name, node.file_path, location
            ));
        }
    } else if remaining.len() > 10 {
        lines.push(format!("... and {} more related symbols", remaining.len()));
    }

    lines.join("\n").trim().to_string()
}

/// Format a single node and its relationships
fn format_node_tree(
    node: &Node,
    subgraph: &Subgraph,
    outgoing: &HashMap<&str, Vec<&Edge>>,
    printed: &mut HashSet<String>,
    lines: &mut Vec<String>,
    depth: u32,
    prefix: &str,
) {
    if printed.contains(&node.id) {
        return;
    }
    printed.insert(node.id.clone());

    // Node header
    let location = if node.start_line != 0 {
        format!(":{}", node.start_line)
    } else {
        String::new()
    };
    let signature = match node.signature.as_deref() {
        Some(sig) if !sig.is_empty() => format!(" - {}", truncate(sig, 50)),
        _ => String::new(),
    };
    lines.push(format!(
        "{}{}: {} ({}{}){}",
        prefix, node.kind, node.name, node.file_path, location, signature
    ));

    // Outgoing edges
    const SIGNIFICANT: [EdgeKind; 5] = [
        EdgeKind::Calls,
        EdgeKind::Extends,
        EdgeKind::Implements,
        EdgeKind::Imports,
        EdgeKind::References,
    ];
    let empty: Vec<&Edge> = Vec::new();
    let edges = outgoing.get(node.id.as_str()).unwrap_or(&empty);
    let significant_edges: Vec<&Edge> = edges
        .iter()
        .filter(|e| SIGNIFICANT.contains(&e.kind))
        .copied()
        .collect();

    // Group by kind (first-seen order, mirrors the TS Map)
    let mut kind_order: Vec<EdgeKind> = Vec::new();
    let mut edges_by_kind: HashMap<EdgeKind, Vec<&Edge>> = HashMap::new();
    for edge in &significant_edges {
        let entry = edges_by_kind.entry(edge.kind).or_insert_with(|| {
            kind_order.push(edge.kind);
            Vec::new()
        });
        entry.push(edge);
    }

    // Print edges grouped by kind
    let new_prefix = format!("{prefix}  ");
    for kind in kind_order {
        let kind_edges = &edges_by_kind[&kind];
        if kind_edges.len() > 3 {
            // Summarize if too many
            let names = kind_edges
                .iter()
                .take(3)
                .map(|e| {
                    subgraph
                        .nodes
                        .get(&e.target)
                        .map(|t| t.name.as_str())
                        .unwrap_or("unknown")
                })
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!(
                "{}├── {}: {} and {} more",
                new_prefix,
                kind,
                names,
                kind_edges.len() - 3
            ));
        } else {
            for (i, edge) in kind_edges.iter().enumerate() {
                let target_name = subgraph
                    .nodes
                    .get(&edge.target)
                    .map(|t| t.name.as_str())
                    .unwrap_or("unknown");
                let connector = if i == kind_edges.len() - 1 {
                    "└──"
                } else {
                    "├──"
                };
                lines.push(format!("{new_prefix}{connector} {kind} → {target_name}"));
            }
        }
    }

    // Recurse for directly connected nodes (limited depth)
    if depth < 1 {
        for edge in significant_edges.iter().take(3) {
            if let Some(target) = subgraph.nodes.get(&edge.target) {
                if !printed.contains(&target.id) {
                    format_node_tree(
                        target,
                        subgraph,
                        outgoing,
                        printed,
                        lines,
                        depth + 1,
                        &new_prefix,
                    );
                }
            }
        }
    }
}

/// Truncate a string with ellipsis (UTF-16 length semantics, like JS).
fn truncate(s: &str, max_length: usize) -> String {
    if s.encode_utf16().count() <= max_length {
        return s.to_string();
    }
    let mut out = String::new();
    let mut units = 0usize;
    for c in s.chars() {
        let len = c.len_utf16();
        if units + len > max_length.saturating_sub(3) {
            break;
        }
        units += len;
        out.push(c);
    }
    out.push_str("...");
    out
}

/// Format bytes as human-readable string
pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} bytes")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Subgraph;

    fn node(id: &str, kind: NodeKind, name: &str, file: &str, line: u32) -> Node {
        Node::new(
            id,
            kind,
            name,
            format!("{file}::{name}"),
            file,
            Language::Typescript,
            line,
            line + 5,
        )
    }

    fn make_context() -> TaskContext {
        let entry = node(
            "n1",
            NodeKind::Class,
            "PaymentService",
            "src/payment.ts",
            10,
        );
        let other = node(
            "n2",
            NodeKind::Method,
            "processPayment",
            "src/payment.ts",
            14,
        );
        let mut nodes = std::collections::HashMap::new();
        nodes.insert(entry.id.clone(), entry.clone());
        nodes.insert(other.id.clone(), other.clone());
        let edge = Edge::new("n1", "n2", EdgeKind::Contains);
        TaskContext {
            query: "payment".to_string(),
            subgraph: Subgraph {
                nodes,
                edges: vec![edge],
                roots: vec!["n1".to_string()],
                confidence: None,
            },
            entry_points: vec![entry],
            code_blocks: vec![],
            related_files: vec!["src/payment.ts".to_string()],
            summary: "summary".to_string(),
            stats: TaskContextStats {
                node_count: 2,
                edge_count: 1,
                file_count: 1,
                code_block_count: 0,
                total_code_size: 0,
            },
        }
    }

    #[test]
    fn markdown_has_header_query_entry_points_and_related_symbols() {
        let md = format_context_as_markdown(&make_context());
        assert!(md.starts_with("## Code Context\n\n**Query:** payment\n"));
        assert!(md.contains("### Entry Points\n"));
        assert!(md.contains("- **PaymentService** (class) - src/payment.ts:10"));
        assert!(md.contains("### Related Symbols\n"));
        assert!(md.contains("- src/payment.ts: processPayment:14"));
    }

    #[test]
    fn json_matches_ts_field_order_and_omits_undefined() {
        let json = format_context_as_json(&make_context());
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["query"], "payment");
        assert_eq!(
            v["entryPoints"][0]["qualifiedName"],
            "src/payment.ts::PaymentService"
        );
        // Key order is the TS serializeNode literal order.
        let keys: Vec<&String> = v["entryPoints"][0].as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            [
                "id",
                "kind",
                "name",
                "qualifiedName",
                "filePath",
                "language",
                "startLine",
                "endLine"
            ]
            .iter()
            .collect::<Vec<_>>()
        );
        // Undefined optionals are omitted entirely.
        assert!(v["entryPoints"][0].get("signature").is_none());
        assert_eq!(v["edges"][0]["kind"], "contains");
        assert!(v["edges"][0].get("line").is_none());
        assert_eq!(v["stats"]["nodeCount"], 2);
    }

    #[test]
    fn format_bytes_matches_ts_strings() {
        assert_eq!(format_bytes(512), "512 bytes");
        assert_eq!(format_bytes(2048), "2.0 KB");
        assert_eq!(format_bytes(3 * 1024 * 1024), "3.0 MB");
    }

    #[test]
    fn truncate_uses_ellipsis() {
        assert_eq!(truncate("short", 50), "short");
        let long = "a".repeat(60);
        let t = truncate(&long, 50);
        assert!(t.ends_with("..."));
        assert_eq!(t.encode_utf16().count(), 50);
    }
}

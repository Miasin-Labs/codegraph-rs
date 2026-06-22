//! Markdown rendering for context / caller / callee / impact results.
//!
//! All output goes through this module so the agent gets a consistent
//! shape: `## Header`, `**Location:**`, fenced code blocks tagged
//! `rust`, file-grouped impact, and a `--- handles ---` footer for
//! chained queries.

mod impact;
mod labels;
mod relationships;
mod sections;

pub use impact::render_impact;
pub(crate) use labels::line_range;
pub use labels::{edge_kind_label, kind_label, visibility_label};
use labels::{line_suffix, signature_for, symbol_label};
use sections::{push_code_blocks, push_entry_points, push_related_symbols};

use crate::context::budget::ExploreBudget;
use crate::context::heuristics::{TaskIntent, reminder_for};
use crate::edges::EdgeKind;
use crate::graph::CodeGraph;
use crate::nodes::{NodeId, NodeKind};
use crate::symbols::SymbolTable;

/// Render a search-result list — one entry per node with kind,
/// location, signature, and a fenced docstring teaser.
pub fn render_search_results(
    graph: &CodeGraph,
    symbols: Option<&SymbolTable>,
    query: &str,
    nodes: &[NodeId],
    note: Option<&str>,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("## Search Results ({} found)\n\n", nodes.len()));
    if nodes.is_empty() {
        out.push_str(&format!("No results for `{query}`.\n"));
        return out;
    }
    let mut any_function = false;
    for id in nodes {
        let Some(node) = graph.get_node(id) else {
            continue;
        };
        if node.kind == NodeKind::Function {
            any_function = true;
        }
        out.push_str(&format!(
            "### {} ({})\n",
            symbol_label(node),
            kind_label(node.kind)
        ));
        out.push_str(&format!(
            "{}{}\n",
            node.file_path.display(),
            line_range(node)
        ));
        let sig = signature_for(node);
        if !sig.is_empty() {
            out.push_str(&format!("`{sig}`\n"));
        }
        if let Some(handle) = symbols.and_then(|s| s.handle_for_node(id)) {
            out.push_str(&format!("handle: `{handle}`\n"));
        }
        let vis = visibility_label(&node.visibility);
        if !vis.is_empty() {
            out.push_str(&format!("visibility: {vis}\n"));
        }
        out.push('\n');
    }
    if let Some(n) = note {
        out.push_str(&format!("\n> **Note:** {n}\n"));
    }
    if any_function {
        out.push_str(
            "\n> 💡 For usage: `graph_callers(\"name\")` / `graph_callees(\"name\")`; \
             for full source: `graph_search` with `include_code=true` or `graph_node`.\n",
        );
    }
    out
}

/// Render a callers / callees list — compact one-line-per-result so
/// many entries fit. Includes signature.
pub fn render_node_list(
    graph: &CodeGraph,
    title: &str,
    nodes: &[NodeId],
    note: Option<&str>,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("## {title} ({} found)\n\n", nodes.len()));
    for id in nodes {
        let Some(node) = graph.get_node(id) else {
            continue;
        };
        let sig = signature_for(node);
        let sig_suffix = if sig.is_empty() {
            String::new()
        } else {
            format!(" — `{sig}`")
        };
        out.push_str(&format!(
            "- {} ({}) — {}{}{}\n",
            symbol_label(node),
            kind_label(node.kind),
            node.file_path.display(),
            line_suffix(node),
            sig_suffix,
        ));
    }
    if let Some(n) = note {
        out.push_str(&format!("\n> **Note:** {n}\n"));
    }
    out
}

/// Render full `codegraph_context`-style output: entry points,
/// related symbols (grouped by file), code blocks for the entry
/// points themselves (caller's responsibility to feed them in).
pub fn render_context(
    graph: &CodeGraph,
    query: &str,
    entry_points: &[NodeId],
    related: &[NodeId],
    code_blocks: &[(NodeId, String)],
    intent: TaskIntent,
    budget: &ExploreBudget,
) -> String {
    let mut out = String::new();
    out.push_str("## Code Context\n\n");
    out.push_str(&format!("**Query:** {query}\n\n"));
    push_entry_points(&mut out, graph, entry_points);
    push_related_symbols(&mut out, graph, related);
    relationships::push_relationships(&mut out, graph, entry_points, related, budget);
    push_code_blocks(&mut out, graph, code_blocks);
    out.push_str(reminder_for(intent));
    if budget.include_completeness_signal && !code_blocks.is_empty() {
        out.push_str(&format!(
            "\n\n---\n> **Complete source code is included above for {} symbols.**\n",
            code_blocks.len(),
        ));
    }
    out
}

/// Render an explore-style payload: a relationships map plus per-file
/// source slices. The caller pre-builds `file_blocks` as
/// `(path, language, header_symbols, body_with_line_numbers)`.
pub fn render_explore(
    query: &str,
    total_symbols: usize,
    total_files: usize,
    relationships: &[(EdgeKind, Vec<(String, String)>)],
    file_blocks: &[(String, String, String, String)],
    additional_files: &[(String, String)],
    overload_notes: &[String],
    semantic_notes: &[String],
    budget: &ExploreBudget,
) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("## Exploration: {query}"));
    lines.push(String::new());
    lines.push(format!(
        "Found {total_symbols} symbols across {total_files} files."
    ));
    lines.push(String::new());

    if budget.include_relationships && !relationships.is_empty() {
        lines.push("### Relationships".into());
        lines.push(String::new());
        for (kind, edges) in relationships {
            lines.push(format!("**{}:**", edge_kind_label(kind)));
            for (src, tgt) in edges.iter().take(budget.max_edges_per_relationship_kind) {
                lines.push(format!("- {src} → {tgt}"));
            }
            if edges.len() > budget.max_edges_per_relationship_kind {
                lines.push(format!(
                    "- ... and {} more",
                    edges.len() - budget.max_edges_per_relationship_kind
                ));
            }
            lines.push(String::new());
        }
    }

    if !overload_notes.is_empty() {
        lines.push("### Matched Symbols".into());
        lines.push(String::new());
        lines.extend(overload_notes.iter().cloned());
        lines.push(String::new());
    }

    if !semantic_notes.is_empty() {
        lines.push("### Semantic Enrichment".into());
        lines.push(String::new());
        lines.extend(semantic_notes.iter().cloned());
        lines.push(String::new());
    }

    if !file_blocks.is_empty() {
        lines.push("### Source Code".into());
        lines.push(String::new());
        for (path, lang, header, body) in file_blocks {
            lines.push(format!("#### {path} — {header}"));
            lines.push(String::new());
            lines.push(format!("```{lang}"));
            lines.push(body.trim_end().to_string());
            lines.push("```".into());
            lines.push(String::new());
        }
    }

    if budget.include_additional_files && !additional_files.is_empty() {
        lines.push("### Additional relevant files (not shown)".into());
        lines.push(String::new());
        for (path, symbols) in additional_files.iter().take(10) {
            lines.push(format!("- {path}: {symbols}"));
        }
        if additional_files.len() > 10 {
            lines.push(format!(
                "- ... and {} more files",
                additional_files.len() - 10
            ));
        }
    }

    if budget.include_completeness_signal && !file_blocks.is_empty() {
        lines.push(String::new());
        lines.push("---".into());
        lines.push(format!(
            "> **Complete source code is included above for {} files.** \
             Use Read only for files under 'Additional relevant files' if you need more detail.",
            file_blocks.len(),
        ));
    }

    if budget.include_budget_note {
        lines.push(String::new());
        lines.push(format!(
            "> **Explore budget: {} calls max for this project.** \
             Stop exploring and synthesise your answer once you've used the budget.",
            budget.recommended_call_budget,
        ));
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests;

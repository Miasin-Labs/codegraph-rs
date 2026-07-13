use std::collections::{HashMap, HashSet};

use super::super::context::ToolHandler;
use super::super::format::{
    ExploreOutputBudget,
    OrderedNodeMap,
    display_symbol,
    floor_char_boundary,
    get_explore_budget,
    output_char_cap,
    to_locale_string,
};
use super::super::schema::{ToolContent, ToolResult};
use super::types::RankedExploreFiles;
use crate::codegraph::CodeGraph;
use crate::error::Result;
use crate::types::{Edge, EdgeKind};

pub(in crate::mcp::tools::explore) fn append_graph_sections(
    handler: &ToolHandler,
    cg: &CodeGraph,
    roots: &[String],
    edges: &[Edge],
    nodes: &OrderedNodeMap,
    named_seed_ids: &HashSet<String>,
    budget: ExploreOutputBudget,
    lines: &mut Vec<String>,
) {
    let mut blast_roots: Vec<String> = named_seed_ids.iter().cloned().collect();
    blast_roots.sort();
    blast_roots.extend(
        roots
            .iter()
            .filter(|id| !named_seed_ids.contains(*id))
            .cloned(),
    );
    let blast_radius = handler.build_blast_radius_section(cg, &blast_roots, nodes);
    if !blast_radius.is_empty() {
        lines.push(blast_radius);
    }

    let significant_edges: Vec<&Edge> = edges
        .iter()
        .filter(|e| e.kind != EdgeKind::Contains)
        .collect();
    if !budget.include_relationships || significant_edges.is_empty() {
        return;
    }
    lines.push("### Relationships".to_string());
    lines.push(String::new());

    let mut kind_order: Vec<String> = Vec::new();
    let mut by_kind: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for edge in &significant_edges {
        let (Some(source_node), Some(target_node)) =
            (nodes.get(&edge.source), nodes.get(&edge.target))
        else {
            continue;
        };
        let kind = edge.kind.as_str().to_string();
        if !by_kind.contains_key(&kind) {
            kind_order.push(kind.clone());
        }
        by_kind
            .entry(kind)
            .or_default()
            .push((display_symbol(source_node), display_symbol(target_node)));
    }

    for kind in &kind_order {
        let group = &by_kind[kind];
        let cap = budget.max_edges_per_relationship_kind;
        lines.push(format!("**{kind}:**"));
        for (source, target) in group.iter().take(cap) {
            lines.push(format!("- {source} → {target}"));
        }
        if group.len() > cap {
            lines.push(format!("- ... and {} more", group.len() - cap));
        }
        lines.push(String::new());
    }
}

pub(in crate::mcp::tools::explore) fn append_remaining_files(
    budget: ExploreOutputBudget,
    ranked: &RankedExploreFiles,
    files_included: usize,
    lines: &mut Vec<String>,
) {
    if !budget.include_additional_files {
        return;
    }
    let remaining_relevant: Vec<String> = ranked
        .sorted_files
        .iter()
        .skip(files_included)
        .cloned()
        .collect();
    let mut peripheral_files: Vec<String> = ranked
        .file_order
        .iter()
        .filter(|fp| ranked.file_groups[*fp].score < 3)
        .cloned()
        .collect();
    peripheral_files.sort_by(|a, b| {
        ranked.file_groups[b]
            .score
            .cmp(&ranked.file_groups[a].score)
    });
    let remaining_files: Vec<String> = remaining_relevant
        .into_iter()
        .chain(peripheral_files)
        .collect();
    if remaining_files.is_empty() {
        return;
    }
    lines.push("### Not shown above — explore these names for their source".to_string());
    lines.push(String::new());
    for file_path in remaining_files.iter().take(10) {
        let group = &ranked.file_groups[file_path];
        let symbols = group
            .nodes
            .iter()
            .map(|n| format!("{}:{}", display_symbol(n), n.start_line))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("- {file_path}: {symbols}"));
    }
    if remaining_files.len() > 10 {
        lines.push(format!(
            "- ... and {} more files",
            remaining_files.len() - 10
        ));
    }
}

pub(in crate::mcp::tools::explore) fn append_explore_footer(
    handler: &ToolHandler,
    cg: &CodeGraph,
    budget: ExploreOutputBudget,
    files_included: usize,
    any_file_trimmed: bool,
    lines: &mut Vec<String>,
) {
    if budget.include_completeness_signal {
        lines.push(String::new());
        lines.push("---".to_string());
        lines.push(format!(
            "> Source included for {files_included} file(s). Query specific omitted names if needed."
        ));
    } else if any_file_trimmed {
        lines.push(String::new());
        lines
            .push("> Some sections were trimmed. Query exact symbols for more detail.".to_string());
    }
    if !budget.include_budget_note {
        return;
    }
    if let Ok(stats) = cg.get_stats() {
        let call_budget = get_explore_budget(stats.file_count);
        lines.push(String::new());
        lines.push(format!(
            "> Explore budget: {} call(s) for this project ({} files indexed). Synthesize after {} call(s).",
            call_budget,
            to_locale_string(stats.file_count),
            call_budget
        ));
    }
    let _ = handler;
}

pub(in crate::mcp::tools::explore) fn finish_explore_result(
    _handler: &ToolHandler,
    flow_text: &str,
    lines: Vec<String>,
    structured_content: Option<serde_json::Value>,
) -> Result<ToolResult> {
    let output = format!("{}{}", flow_text, lines.join("\n"));
    if let Some(cap) = output_char_cap() {
        if output.len() > cap {
            const TRUNCATION_SUFFIX_RESERVE: usize = 250;
            let cut_at = cap.saturating_sub(TRUNCATION_SUFFIX_RESERVE);
            let cut = &output[..floor_char_boundary(&output, cut_at)];
            let last_section = cut.rfind("\n#### ");
            let boundary = match last_section {
                Some(pos) if (pos as f64) > cut_at as f64 * 0.5 => Some(pos),
                _ => cut.rfind('\n'),
            };
            let safe = match boundary {
                Some(pos) if pos > 0 => &cut[..pos],
                _ => cut,
            };
            let text = format!(
                "{safe}\n\n... (output truncated to budget; query specific names for omitted areas.)"
            );
            return Ok(ToolResult {
                content: vec![ToolContent {
                    content_type: "text".into(),
                    text,
                }],
                structured_content,
                meta: None,
                is_error: None,
            });
        }
    }
    Ok(ToolResult {
        content: vec![ToolContent {
            content_type: "text".into(),
            text: output,
        }],
        structured_content,
        meta: None,
        is_error: None,
    })
}

use std::collections::HashSet;

use super::super::format::{ExploreOutputBudget, FlowInfo, display_symbol, number_source_lines};
use super::types::{FileGroup, SourceChunk, SourceChunkMode};
use crate::types::{Node, NodeKind};

pub(in crate::mcp::tools::explore) struct SkeletonSelection {
    pub lines: Vec<String>,
    pub chunks: Vec<SourceChunk>,
}

pub(in crate::mcp::tools::explore) fn render_skeleton(
    syms: &[&Node],
    file_lines: &[&str],
    flow: &FlowInfo,
    budget: ExploreOutputBudget,
    with_line_numbers: bool,
    body_ids: &HashSet<String>,
) -> SkeletonSelection {
    let mut skel = Vec::new();
    let mut chunks = Vec::new();
    let mut covered_until = 0i64;
    let mut sig_count = 0usize;
    let mut sig_dropped = 0usize;
    let sig_max = 12usize.max(budget.max_symbols_in_file_header * 2);
    for n in syms {
        if (n.start_line as i64) <= covered_until {
            continue;
        }
        if body_ids.contains(&n.id) {
            let end = n.end_line as i64;
            let Some(chunk) = SourceChunk::from_lines(
                file_lines,
                n.start_line as i64,
                end,
                SourceChunkMode::Body,
                vec![format!("{}({})", display_symbol(n), n.kind.as_str())],
            ) else {
                continue;
            };
            skel.push(if with_line_numbers {
                number_source_lines(&chunk.source, chunk.start_line)
            } else {
                chunk.source.clone()
            });
            chunks.push(chunk);
            covered_until = end;
            continue;
        }

        let mut line_no = n.start_line as i64;
        for k in 0..4i64 {
            let idx = n.start_line as i64 - 1 + k;
            let line = if idx >= 0 && (idx as usize) < file_lines.len() {
                file_lines[idx as usize]
            } else {
                ""
            };
            if line.contains(&n.name) {
                line_no = n.start_line as i64 + k;
                break;
            }
        }
        if line_no <= covered_until {
            continue;
        }
        if sig_count >= sig_max {
            sig_dropped += 1;
            continue;
        }
        let sig = if line_no >= 1 && (line_no as usize) <= file_lines.len() {
            file_lines[line_no as usize - 1].trim()
        } else {
            ""
        };
        if !sig.is_empty() {
            let Some(chunk) = SourceChunk::from_lines(
                file_lines,
                line_no,
                line_no,
                SourceChunkMode::Signature,
                vec![format!("{}({})", display_symbol(n), n.kind.as_str())],
            ) else {
                continue;
            };
            skel.push(if with_line_numbers {
                format!("{line_no}\t{sig}")
            } else {
                sig.to_string()
            });
            chunks.push(chunk);
            sig_count += 1;
        }
    }
    if sig_dropped > 0 {
        skel.push(format!("… +{sig_dropped} more (signatures elided)"));
    }
    let _ = flow;
    SkeletonSelection {
        lines: skel,
        chunks,
    }
}

pub(in crate::mcp::tools::explore) fn adaptive_header_names(
    group: &FileGroup,
    budget: ExploreOutputBudget,
) -> String {
    let mut name_seen = HashSet::new();
    group
        .nodes
        .iter()
        .filter(|n| n.kind != NodeKind::Import && n.kind != NodeKind::Export)
        .filter_map(|n| {
            let label = display_symbol(n);
            name_seen.insert(label.clone()).then_some(label)
        })
        .take(budget.max_symbols_in_file_header)
        .collect::<Vec<_>>()
        .join(", ")
}

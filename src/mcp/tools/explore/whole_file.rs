use std::collections::HashSet;

use super::super::format::{ExploreOutputBudget, display_symbol, number_source_lines};
use super::types::{FileGroup, RenderedFile, SourceChunk, SourceChunkMode};
use crate::types::NodeKind;

pub(in crate::mcp::tools::explore) struct WholeFileRequest<'a> {
    pub file_path: &'a str,
    pub group: &'a FileGroup,
    pub file_content: &'a str,
    pub file_lines: &'a [&'a str],
    pub language: &'a str,
    pub budget: ExploreOutputBudget,
    pub total_chars: usize,
    pub is_central_file: bool,
    pub with_line_numbers: bool,
}

pub(in crate::mcp::tools::explore) fn render_whole_file(
    req: WholeFileRequest<'_>,
) -> Option<RenderedFile> {
    let whole_file_max_lines = if req.is_central_file { 280 } else { 220 };
    let whole_file_max_chars = if req.is_central_file {
        (req.budget.max_output_chars as i64 - req.total_chars as i64 - 200)
            .max(0)
            .min((req.budget.max_chars_per_file as f64 * 1.5).round() as i64) as usize
    } else {
        req.budget.max_chars_per_file * 3
    };
    if req.file_lines.len() > whole_file_max_lines || req.file_content.len() > whole_file_max_chars
    {
        linkscope::event_fields(
            "codegraph.explore.whole_file_skipped",
            [
                linkscope::TraceField::text("file", req.file_path),
                linkscope::TraceField::count("lines", req.file_lines.len() as u64),
                linkscope::TraceField::count("max_lines", whole_file_max_lines as u64),
                linkscope::TraceField::bytes("chars", req.file_content.len() as u64),
                linkscope::TraceField::bytes("max_chars", whole_file_max_chars as u64),
            ],
        );
        return None;
    }

    let body = req.file_content.trim_end_matches('\n');
    let whole_section = if req.with_line_numbers {
        number_source_lines(body, 1)
    } else {
        body.to_string()
    };
    let mut sym_seen = HashSet::new();
    let uniq_symbols: Vec<String> = req
        .group
        .nodes
        .iter()
        .filter(|n| n.kind != NodeKind::Import && n.kind != NodeKind::Export)
        .map(|n| format!("{}({})", display_symbol(n), n.kind.as_str()))
        .filter(|s| sym_seen.insert(s.clone()))
        .collect();
    let header_names: Vec<String> = uniq_symbols
        .iter()
        .take(req.budget.max_symbols_in_file_header)
        .cloned()
        .collect();
    let omitted = uniq_symbols.len() as i64 - header_names.len() as i64;
    let header = if omitted > 0 {
        format!(
            "#### {} — {}, +{} more",
            req.file_path,
            header_names.join(", "),
            omitted
        )
    } else {
        format!("#### {} — {}", req.file_path, header_names.join(", "))
    };
    let selected_end = req.file_lines.len().saturating_sub(
        req.file_lines
            .iter()
            .rev()
            .take_while(|line| line.is_empty())
            .count(),
    );
    let chunks = SourceChunk::from_lines(
        req.file_lines,
        1,
        selected_end as i64,
        SourceChunkMode::Whole,
        uniq_symbols,
    )
    .into_iter()
    .collect();
    let cost = whole_section.len() + 200;
    Some(RenderedFile {
        header,
        language: req.language.to_string(),
        body: whole_section,
        chunks,
        cost,
    })
}

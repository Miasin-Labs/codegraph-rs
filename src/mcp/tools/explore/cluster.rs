mod ranges;
mod selection;

use ranges::collect_ranges;
use selection::{cluster_header, render_selected_clusters};

use super::super::format::{ExploreOutputBudget, FlowInfo, OrderedNodeMap};
use super::types::{FileGroup, RenderedFile};
use crate::codegraph::CodeGraph;
use crate::error::Result;

pub(in crate::mcp::tools::explore) struct ClusterRequest<'a> {
    pub cg: &'a CodeGraph,
    pub file_path: &'a str,
    pub group: &'a FileGroup,
    pub file_lines: &'a [&'a str],
    pub language: &'a str,
    pub nodes: &'a OrderedNodeMap,
    pub glue_node_ids: &'a std::collections::HashSet<String>,
    pub flow: &'a FlowInfo,
    pub budget: ExploreOutputBudget,
    pub entry_node_ids: &'a std::collections::HashSet<String>,
    pub connected_to_entry: &'a std::collections::HashSet<String>,
    pub total_chars: usize,
    pub with_line_numbers: bool,
}

pub(in crate::mcp::tools::explore) fn render_clustered_file(
    req: ClusterRequest<'_>,
) -> Result<Option<RenderedFile>> {
    let mut ranges = collect_ranges(&req)?;
    ranges.sort_by_key(|r| r.start);
    if ranges.is_empty() {
        return Ok(None);
    }

    let Some(selection) = render_selected_clusters(
        &ranges,
        req.file_lines,
        req.budget,
        req.total_chars,
        req.with_line_numbers,
    ) else {
        return Ok(None);
    };
    let header = format!(
        "#### {} — {}",
        req.file_path,
        cluster_header(&selection.symbols, req.budget)
    );
    let cost = selection.body.len() + 200;
    Ok(Some(RenderedFile {
        header,
        language: req.language.to_string(),
        body: selection.body,
        chunks: selection.chunks,
        cost,
    }))
}

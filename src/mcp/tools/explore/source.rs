use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::super::format::{ExploreOutputBudget, FlowInfo, OrderedNodeMap};
use super::adaptive::{AdaptiveRequest, render_adaptive_section};
use super::cluster::{ClusterRequest, render_clustered_file};
use super::types::{
    OmissionReason,
    OmittedFile,
    RankedExploreFiles,
    RenderedFile,
    StructuredSourceFile,
};
use super::whole_file::{WholeFileRequest, render_whole_file};
use crate::codegraph::CodeGraph;
use crate::error::Result;
use crate::utils::validate_path_within_root;

pub(in crate::mcp::tools::explore) struct SourceFilesRequest<'a> {
    pub cg: &'a CodeGraph,
    pub project_root: &'a Path,
    pub ranked: &'a RankedExploreFiles,
    pub nodes: &'a OrderedNodeMap,
    pub glue_node_ids: &'a std::collections::HashSet<String>,
    pub flow: &'a FlowInfo,
    pub budget: ExploreOutputBudget,
    pub max_files: usize,
    pub with_line_numbers: bool,
    pub initial_chars: usize,
}

pub(in crate::mcp::tools::explore) struct SourceFilesResult {
    pub files_included: usize,
    pub any_file_trimmed: bool,
    pub rendered_files: Vec<StructuredSourceFile>,
    pub omissions: Vec<OmittedFile>,
}

pub(in crate::mcp::tools::explore) fn render_source_files(
    req: SourceFilesRequest<'_>,
    lines: &mut Vec<String>,
) -> Result<SourceFilesResult> {
    let mut sibling_super = HashMap::new();
    let mut super_many = HashMap::new();
    let mut total_chars = req.initial_chars;
    let mut files_included = 0usize;
    let mut any_file_trimmed = false;
    let mut rendered_files = Vec::new();
    let mut omissions = Vec::new();

    for (idx, file_path) in req.ranked.sorted_files.iter().enumerate() {
        if files_included >= req.max_files {
            omissions.extend(
                req.ranked.sorted_files[idx..]
                    .iter()
                    .map(|path| omitted_file(req.ranked, path, OmissionReason::MaxFiles)),
            );
            break;
        }
        let group = &req.ranked.file_groups[file_path];
        let file_necessary = group.nodes.iter().any(|n| {
            req.ranked.entry_node_ids.contains(&n.id)
                || req.flow.path_node_ids.contains(&n.id)
                || req.flow.unique_named_node_ids.contains(&n.id)
        });
        if !file_necessary && total_chars as f64 > req.budget.max_output_chars as f64 * 0.9 {
            omissions.push(omitted_file(req.ranked, file_path, OmissionReason::Budget));
            continue;
        }

        let Some(abs_path) = validate_path_within_root(req.project_root, file_path) else {
            omissions.push(omitted_file(
                req.ranked,
                file_path,
                OmissionReason::Unavailable,
            ));
            continue;
        };
        if !abs_path.exists() {
            omissions.push(omitted_file(
                req.ranked,
                file_path,
                OmissionReason::Unavailable,
            ));
            continue;
        }
        let Ok(file_content) = std::fs::read_to_string(&abs_path) else {
            omissions.push(omitted_file(
                req.ranked,
                file_path,
                OmissionReason::Unavailable,
            ));
            continue;
        };
        let file_lines: Vec<&str> = file_content.split('\n').collect();
        let language = group
            .nodes
            .first()
            .map(|n| n.language.as_str())
            .unwrap_or("");

        if let Some(rendered) = render_adaptive_section(AdaptiveRequest {
            cg: req.cg,
            file_path,
            group,
            file_lines: &file_lines,
            language,
            flow: req.flow,
            budget: req.budget,
            with_line_numbers: req.with_line_numbers,
            sibling_super: &mut sibling_super,
            super_many: &mut super_many,
        })? {
            let (source_file, cost) = append_rendered(lines, file_path, rendered);
            rendered_files.push(source_file);
            total_chars += cost;
            files_included += 1;
            continue;
        }

        if let Some(rendered) = render_whole_file(WholeFileRequest {
            file_path,
            group,
            file_content: &file_content,
            file_lines: &file_lines,
            language,
            budget: req.budget,
            total_chars,
            is_central_file: req.ranked.central_files.contains(file_path),
            with_line_numbers: req.with_line_numbers,
        }) {
            if !file_necessary && total_chars + rendered.cost > req.budget.max_output_chars {
                any_file_trimmed = true;
                omissions.push(omitted_file(req.ranked, file_path, OmissionReason::Budget));
                continue;
            }
            let (source_file, cost) = append_rendered(lines, file_path, rendered);
            rendered_files.push(source_file);
            total_chars += cost;
            files_included += 1;
            continue;
        }

        if let Some(rendered) = render_clustered_file(ClusterRequest {
            cg: req.cg,
            file_path,
            group,
            file_lines: &file_lines,
            language,
            nodes: req.nodes,
            glue_node_ids: req.glue_node_ids,
            flow: req.flow,
            budget: req.budget,
            entry_node_ids: &req.ranked.entry_node_ids,
            connected_to_entry: &req.ranked.connected_to_entry,
            total_chars,
            with_line_numbers: req.with_line_numbers,
        })? {
            if !file_necessary && total_chars + rendered.cost > req.budget.max_output_chars {
                any_file_trimmed = true;
                omissions.push(omitted_file(req.ranked, file_path, OmissionReason::Budget));
                continue;
            }
            let (source_file, cost) = append_rendered(lines, file_path, rendered);
            rendered_files.push(source_file);
            total_chars += cost;
            files_included += 1;
            continue;
        }

        omissions.push(omitted_file(
            req.ranked,
            file_path,
            OmissionReason::NoSource,
        ));
    }

    Ok(SourceFilesResult {
        files_included,
        any_file_trimmed,
        rendered_files,
        omissions,
    })
}

fn append_rendered(
    lines: &mut Vec<String>,
    file_path: &str,
    rendered: RenderedFile,
) -> (StructuredSourceFile, usize) {
    let cost = rendered.cost;
    lines.push(rendered.header.clone());
    lines.push(String::new());
    lines.push(format!("```{}", rendered.language));
    lines.push(rendered.body.clone());
    lines.push("```".to_string());
    lines.push(String::new());
    (rendered.into_structured(file_path), cost)
}

fn omitted_file(
    ranked: &RankedExploreFiles,
    file_path: &str,
    reason: OmissionReason,
) -> OmittedFile {
    let mut seen = HashSet::new();
    let symbols = ranked
        .file_groups
        .get(file_path)
        .map(|group| {
            group
                .nodes
                .iter()
                .map(|node| node.name.clone())
                .filter(|name| !name.is_empty() && seen.insert(name.clone()))
                .take(12)
                .collect()
        })
        .unwrap_or_default();
    OmittedFile {
        path: file_path.to_string(),
        reason,
        symbols,
    }
}

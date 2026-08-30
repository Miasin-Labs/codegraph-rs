use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::super::format::{ExploreOutputBudget, FlowInfo, OrderedNodeMap};
use super::adaptive::{AdaptiveRequest, render_adaptive_section};
use super::cluster::{ClusterRequest, render_clustered_file};
use super::types::{
    ExploreBackReference,
    OmissionReason,
    OmittedFile,
    RankedExploreFiles,
    StructuredSourceFile,
};
use super::whole_file::{WholeFileRequest, render_whole_file};
use crate::codegraph::CodeGraph;
use crate::error::Result;
use crate::mcp::explore_session::ProjectState;
use crate::utils::resolve_existing_path_within_root_real;

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
    pub prior: Option<&'a ProjectState>,
}

pub(in crate::mcp::tools::explore) struct SourceFilesResult {
    pub files_included: usize,
    pub any_file_trimmed: bool,
    pub rendered_files: Vec<StructuredSourceFile>,
    pub omissions: Vec<OmittedFile>,
    pub back_references: Vec<ExploreBackReference>,
    pub stale_files: Vec<String>,
}

pub(in crate::mcp::tools::explore) fn render_source_files(
    req: SourceFilesRequest<'_>,
    lines: &mut Vec<String>,
) -> Result<SourceFilesResult> {
    let mut sibling_super = HashMap::new();
    let mut super_many = HashMap::new();
    let mut any_file_trimmed = false;
    let mut omissions = Vec::new();
    let mut stale_files = Vec::new();
    let mut appender = dedup::Appender::new(
        lines,
        dedup::Context {
            root: req.project_root,
            prior: req.prior,
            line_numbers: req.with_line_numbers,
        },
        req.initial_chars,
    );
    let plan = allocation::plan(&req);
    for reference in plan.back_references {
        appender.add_back_reference(reference);
    }
    let held_paths = plan.held_paths;
    let allocation = plan.allocation;
    debug_assert!(allocation.allowances.values().sum::<usize>() <= allocation.pool);
    let mut carry = 0usize;

    for (idx, file_path) in req.ranked.sorted_files.iter().enumerate() {
        crate::graph::cancel::check()?;
        if held_paths.contains(file_path.as_str()) {
            continue;
        }
        let Some(reserved) = allocation.allowances.get(file_path).copied() else {
            let reason = if allocation.slot_limited.contains(file_path) {
                OmissionReason::MaxFiles
            } else if allocation.cliffed.contains(file_path) {
                OmissionReason::Budget
            } else {
                OmissionReason::MaxFiles
            };
            omissions.push(omitted_file(req.ranked, file_path, reason));
            continue;
        };
        if appender.files_included() >= req.max_files {
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
        if !file_necessary
            && appender.total_chars() as f64 > req.budget.max_output_chars as f64 * 0.9
        {
            omissions.push(omitted_file(req.ranked, file_path, OmissionReason::Budget));
            continue;
        }

        let Some(abs_path) = resolve_existing_path_within_root_real(req.project_root, file_path)
        else {
            omissions.push(omitted_file(
                req.ranked,
                file_path,
                OmissionReason::Unavailable,
            ));
            continue;
        };
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
        let funded = reserved.saturating_add(carry);
        let mut file_budget = req.budget;
        file_budget.max_chars_per_file = funded;
        file_budget.max_output_chars = appender
            .total_chars()
            .saturating_add(funded)
            .saturating_add(200);

        let file_stale = req
            .cg
            .get_file(file_path)
            .ok()
            .flatten()
            .is_some_and(|indexed| {
                crate::extraction::hash_content(&file_content) != indexed.content_hash
            });
        if file_stale {
            stale_files.push(file_path.clone());
            if let Some(mut rendered) = render_whole_file(WholeFileRequest {
                file_path,
                group,
                file_content: &file_content,
                file_lines: &file_lines,
                language,
                budget: file_budget,
                total_chars: appender.total_chars(),
                is_central_file: req.ranked.central_files.contains(file_path),
                with_line_numbers: req.with_line_numbers,
            }) {
                rendered.header.push_str(
                    " · ⚠ changed since last index sync — source below is full and current; indexed symbol lines may be shifted",
                );
                let spent = appender.append(file_path, rendered);
                carry = funded.saturating_sub(spent);
            } else {
                appender.append_notice(format!(
                    "#### {file_path} — ⚠ changed on disk after the last index sync — source omitted because indexed line ranges no longer match and a slice could show the wrong code. Read this file directly for current content."
                ));
                any_file_trimmed = true;
                omissions.push(omitted_file(
                    req.ranked,
                    file_path,
                    OmissionReason::StaleIndex,
                ));
            }
            continue;
        }

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
            let spent = appender.append(file_path, rendered);
            carry = funded.saturating_sub(spent);
            continue;
        }

        if let Some(rendered) = render_whole_file(WholeFileRequest {
            file_path,
            group,
            file_content: &file_content,
            file_lines: &file_lines,
            language,
            budget: file_budget,
            total_chars: appender.total_chars(),
            is_central_file: req.ranked.central_files.contains(file_path),
            with_line_numbers: req.with_line_numbers,
        }) {
            if !file_necessary
                && appender.total_chars() + rendered.cost > req.budget.max_output_chars
            {
                any_file_trimmed = true;
                omissions.push(omitted_file(req.ranked, file_path, OmissionReason::Budget));
                continue;
            }
            let spent = appender.append(file_path, rendered);
            carry = funded.saturating_sub(spent);
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
            budget: file_budget,
            entry_node_ids: &req.ranked.entry_node_ids,
            connected_to_entry: &req.ranked.connected_to_entry,
            total_chars: appender.total_chars(),
            with_line_numbers: req.with_line_numbers,
        })? {
            if !file_necessary
                && appender.total_chars() + rendered.cost > req.budget.max_output_chars
            {
                any_file_trimmed = true;
                omissions.push(omitted_file(req.ranked, file_path, OmissionReason::Budget));
                continue;
            }
            let spent = appender.append(file_path, rendered);
            carry = funded.saturating_sub(spent);
            continue;
        }

        omissions.push(omitted_file(
            req.ranked,
            file_path,
            OmissionReason::NoSource,
        ));
    }

    let (files_included, rendered_files, back_references) = appender.finish();

    Ok(SourceFilesResult {
        files_included,
        any_file_trimmed,
        rendered_files,
        omissions,
        back_references,
        stale_files,
    })
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
mod allocation;
mod dedup;

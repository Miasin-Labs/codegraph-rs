use std::collections::HashSet;

use super::super::super::format::is_low_value;
use super::super::allocation::{Allocation, Candidate, allocate};
use super::super::types::ExploreBackReference;
use super::SourceFilesRequest;
use crate::mcp::explore_session::{file_fingerprint, served_ranges};

pub(super) struct Plan {
    pub allocation: Allocation,
    pub held_paths: HashSet<String>,
    pub back_references: Vec<ExploreBackReference>,
}

pub(super) fn plan(req: &SourceFilesRequest<'_>) -> Plan {
    let mut held_paths = HashSet::new();
    if let Some(prior) = req.prior {
        for file_path in &req.ranked.sorted_files {
            let Some(fingerprint) = file_fingerprint(req.project_root, file_path) else {
                continue;
            };
            if !served_ranges(prior, file_path, &fingerprint).is_empty() {
                held_paths.insert(file_path.clone());
            }
        }
    }
    if held_paths.len() == req.ranked.sorted_files.len() {
        if let Some(first) = req.ranked.sorted_files.first() {
            held_paths.remove(first);
        }
    }
    let back_references = req.prior.map_or_else(Vec::new, |prior| {
        req.ranked
            .sorted_files
            .iter()
            .filter(|path| held_paths.contains(*path))
            .filter_map(|path| {
                let fingerprint = file_fingerprint(req.project_root, path)?;
                Some(ExploreBackReference {
                    path: path.clone(),
                    ranges: served_ranges(prior, path, &fingerprint),
                    symbols: req.ranked.file_groups[path]
                        .nodes
                        .iter()
                        .map(|node| node.name.clone())
                        .take(5)
                        .collect(),
                })
            })
            .collect()
    });
    let candidates = req
        .ranked
        .sorted_files
        .iter()
        .filter(|path| !held_paths.contains(*path))
        .map(|path| Candidate {
            path,
            score: req.ranked.file_groups[path].score as f64,
            worth: if req.ranked.generated_files.contains(path) {
                0.3
            } else if is_low_value(path) {
                0.5
            } else {
                1.0
            },
            spine: req.ranked.file_groups[path].nodes.iter().any(|node| {
                req.flow.path_node_ids.contains(&node.id)
                    || req.flow.unique_named_node_ids.contains(&node.id)
                    || req.flow.named_node_ids.contains(&node.id)
            }),
        })
        .collect::<Vec<_>>();
    Plan {
        allocation: allocate(&candidates, req.budget, req.max_files),
        held_paths,
        back_references,
    }
}

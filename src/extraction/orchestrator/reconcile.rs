use std::collections::{HashMap, HashSet};

use super::pipeline::ExtractionOrchestrator;
use super::progress::ReconcileResult;
use super::scan::scan_directory;
use crate::db::QueryBuilder;
use crate::error::Result;
use crate::types::{EdgeKind, Node, UnresolvedReference};

pub(super) fn reference_kind_for_removed_target(edge_kind: EdgeKind) -> Option<EdgeKind> {
    match edge_kind {
        EdgeKind::Contains => None,
        EdgeKind::Instantiates => Some(EdgeKind::Calls),
        other => Some(other),
    }
}

pub(super) fn restore_unresolved_refs_for_removed_targets(
    queries: &QueryBuilder,
    removed_file_path: &str,
    removed_nodes: &[Node],
) -> Result<()> {
    let target_ids: Vec<String> = removed_nodes.iter().map(|n| n.id.clone()).collect();
    if target_ids.is_empty() {
        return Ok(());
    }

    let incoming = queries.get_incoming_edges_for_targets(&target_ids, None)?;
    if incoming.is_empty() {
        return Ok(());
    }

    let target_by_id: HashMap<&str, &Node> = removed_nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    let source_ids: Vec<String> = incoming.iter().map(|edge| edge.source.clone()).collect();
    let source_by_id = queries.get_nodes_by_ids(&source_ids)?;

    let mut seen: HashSet<String> = HashSet::new();
    let mut refs = Vec::new();
    for edge in incoming {
        let Some(reference_kind) = reference_kind_for_removed_target(edge.kind) else {
            continue;
        };
        let Some(source) = source_by_id.get(&edge.source) else {
            continue;
        };
        if source.file_path == removed_file_path {
            continue;
        }
        let Some(target) = target_by_id.get(edge.target.as_str()) else {
            continue;
        };
        let key = format!(
            "{}\0{}\0{}",
            source.id,
            target.name,
            reference_kind.as_str()
        );
        if !seen.insert(key) {
            continue;
        }
        refs.push(UnresolvedReference {
            from_node_id: source.id.clone(),
            reference_name: target.name.clone(),
            reference_kind,
            line: edge.line.unwrap_or(source.start_line),
            column: edge.column.unwrap_or(source.start_column),
            file_path: Some(source.file_path.clone()),
            language: Some(source.language),
            candidates: None,
        });
    }

    queries.insert_unresolved_refs_batch(&refs)
}

impl<'a> ExtractionOrchestrator<'a> {
    pub fn reconcile_removed_files(&self) -> Result<ReconcileResult> {
        let current_files: HashSet<String> =
            scan_directory(&self.root_dir, None).into_iter().collect();
        // TS uses a Set — preserve first-insertion order for the output array.
        let mut removed_node_names: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut files_removed = 0usize;

        for tracked in self.queries.get_all_files()? {
            if current_files.contains(&tracked.path) && self.root_dir.join(&tracked.path).exists() {
                continue;
            }
            for node in self.queries.get_nodes_by_file(&tracked.path)? {
                if seen.insert(node.name.clone()) {
                    removed_node_names.push(node.name);
                }
            }
            self.queries.delete_file(&tracked.path)?;
            files_removed += 1;
        }

        Ok(ReconcileResult {
            files_removed,
            removed_node_names,
        })
    }
}

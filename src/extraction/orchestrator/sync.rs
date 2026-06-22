use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use super::pipeline::ExtractionOrchestrator;
use super::progress::{
    ChangedFiles,
    FileStats,
    IndexPhase,
    IndexProgress,
    SyncResult,
    emit,
    now_ms,
};
use super::reconcile::restore_unresolved_refs_for_removed_targets;
use super::scan::scan_directory;
use super::store::hash_content;
use crate::db::QueryBuilder;
use crate::error::{Result, log_debug};
use crate::extraction::grammars::{detect_language, init_grammars, load_grammars_for_languages};
use crate::types::{FileRecord, Language};

// =============================================================================
// Filesystem-vs-index diff
// =============================================================================

struct FileDiffResult {
    files_checked: usize,
    added: Vec<String>,
    modified: Vec<String>,
    metadata_only: Vec<FileRecord>,
    removed: Vec<String>,
}

/// Compare the current filesystem against the DB's tracked file state.
/// This is the freshness source of truth: git status is not enough because
/// pull/checkout/merge can change files and leave a clean working tree.
fn diff_filesystem_against_index(
    root_dir: &Path,
    queries: &QueryBuilder,
) -> Result<FileDiffResult> {
    let current_files = scan_directory(root_dir, None);
    let current_set: HashSet<&str> = current_files.iter().map(|s| s.as_str()).collect();
    let tracked_files = queries.get_all_files()?;
    let tracked_map: HashMap<&str, &FileRecord> =
        tracked_files.iter().map(|f| (f.path.as_str(), f)).collect();

    let mut added: Vec<String> = Vec::new();
    let mut modified: Vec<String> = Vec::new();
    let mut metadata_only: Vec<FileRecord> = Vec::new();
    let mut removed: Vec<String> = Vec::new();

    for tracked in &tracked_files {
        if !current_set.contains(tracked.path.as_str()) || !root_dir.join(&tracked.path).exists() {
            removed.push(tracked.path.clone());
        }
    }

    for file_path in &current_files {
        let full_path = root_dir.join(file_path);
        let tracked = tracked_map.get(file_path.as_str()).copied();
        let mut current_stats: Option<FileStats> = None;

        if let Some(tracked) = tracked {
            match fs::metadata(&full_path) {
                Ok(stat) => {
                    let stats = FileStats::from_metadata(&stat);
                    if stats.size == tracked.size && stats.modified_at_ms == tracked.modified_at {
                        continue;
                    }
                    current_stats = Some(stats);
                }
                Err(error) => {
                    log_debug(
                        "Skipping unstattable file while detecting changes",
                        Some(&serde_json::json!({
                            "filePath": file_path,
                            "error": error.to_string(),
                        })),
                    );
                    continue;
                }
            }
        }

        let content = match fs::read(&full_path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(error) => {
                log_debug(
                    "Skipping unreadable file while detecting changes",
                    Some(&serde_json::json!({
                        "filePath": file_path,
                        "error": error.to_string(),
                    })),
                );
                continue;
            }
        };

        let content_hash = hash_content(&content);
        match tracked {
            None => added.push(file_path.clone()),
            Some(t) if t.content_hash != content_hash => modified.push(file_path.clone()),
            Some(t) => {
                if let Some(stats) = current_stats {
                    metadata_only.push(FileRecord {
                        path: t.path.clone(),
                        content_hash: t.content_hash.clone(),
                        language: t.language,
                        size: stats.size,
                        modified_at: stats.modified_at_ms,
                        indexed_at: now_ms(),
                        node_count: t.node_count,
                        errors: t.errors.clone(),
                    });
                }
            }
        }
    }

    Ok(FileDiffResult {
        files_checked: current_files.len(),
        added,
        modified,
        metadata_only,
        removed,
    })
}

impl<'a> ExtractionOrchestrator<'a> {
    /// Sync the index with the current file state.
    ///
    /// Change detection is filesystem-based, never git: a (size, mtime) stat
    /// pre-filter skips unchanged files, then a content-hash compare confirms real
    /// changes. This works in non-git projects and catches committed changes from
    /// `git pull`/`checkout`/`merge`/`rebase` that `git status` cannot see.
    pub fn sync(&self, on_progress: Option<&dyn Fn(&IndexProgress)>) -> Result<SyncResult> {
        init_grammars();
        let start_time = now_ms();
        let mut nodes_updated = 0usize;
        let mut changed_file_paths: Vec<String> = Vec::new();
        // TS uses a Set — preserve first-insertion order.
        let mut changed_node_names: Vec<String> = Vec::new();
        let mut changed_seen: HashSet<String> = HashSet::new();

        emit(
            on_progress,
            IndexProgress {
                phase: IndexPhase::Scanning,
                current: 0,
                total: 0,
                current_file: None,
            },
        );

        let diff = diff_filesystem_against_index(&self.root_dir, self.queries)?;
        let files_checked = diff.files_checked;
        let files_added = diff.added.len();
        let files_modified = diff.modified.len();
        let files_removed = diff.removed.len();
        let files_to_index: Vec<String> = diff
            .added
            .iter()
            .chain(diff.modified.iter())
            .cloned()
            .collect();
        changed_file_paths.extend(files_to_index.iter().cloned());

        for file_record in &diff.metadata_only {
            self.queries.upsert_file(file_record)?;
        }

        for file_path in &diff.removed {
            let removed_nodes = self.queries.get_nodes_by_file(file_path)?;
            for node in &removed_nodes {
                if changed_seen.insert(node.name.clone()) {
                    changed_node_names.push(node.name.clone());
                }
            }
            restore_unresolved_refs_for_removed_targets(self.queries, file_path, &removed_nodes)?;
            self.queries.delete_file(file_path)?;
        }

        // Load only grammars needed for changed files (no-op shim natively)
        if !files_to_index.is_empty() {
            let mut needed_languages: Vec<Language> = Vec::new();
            for f in &files_to_index {
                let lang = detect_language(f, None);
                if !needed_languages.contains(&lang) {
                    needed_languages.push(lang);
                }
            }
            // .h files default to 'c' but may be C++ — ensure cpp grammar is loaded
            if needed_languages.contains(&Language::C) && !needed_languages.contains(&Language::Cpp)
            {
                needed_languages.push(Language::Cpp);
            }
            load_grammars_for_languages(&needed_languages);
        }

        // Index changed files
        let total = files_to_index.len();
        for (i, file_path) in files_to_index.iter().enumerate() {
            emit(
                on_progress,
                IndexProgress {
                    phase: IndexPhase::Parsing,
                    current: i + 1,
                    total,
                    current_file: Some(file_path.clone()),
                },
            );

            let before_file = self.queries.get_file_by_path(file_path)?;
            for node in self.queries.get_nodes_by_file(file_path)? {
                if changed_seen.insert(node.name.clone()) {
                    changed_node_names.push(node.name);
                }
            }

            let result = self.index_file(file_path)?;
            nodes_updated += result.nodes.len();
            for node in &result.nodes {
                if changed_seen.insert(node.name.clone()) {
                    changed_node_names.push(node.name.clone());
                }
            }

            // If a previously-indexed file is now unreadable, too large, or otherwise
            // unindexable, remove its old graph state. Missing beats stale: callers can
            // fall back to direct file reads, but stale symbols make tool answers wrong.
            let after_file = self.queries.get_file_by_path(file_path)?;
            if let (Some(before), Some(after)) = (&before_file, &after_file) {
                if after.content_hash == before.content_hash {
                    self.queries.delete_file(file_path)?;
                }
            }
        }

        Ok(SyncResult {
            files_checked,
            files_added,
            files_modified,
            files_removed,
            nodes_updated,
            duration_ms: now_ms() - start_time,
            changed_file_paths: if changed_file_paths.is_empty() {
                None
            } else {
                Some(changed_file_paths)
            },
            changed_node_names: if changed_node_names.is_empty() {
                None
            } else {
                Some(changed_node_names)
            },
        })
    }

    /// Get files that have changed since last index.
    /// Uses filesystem-vs-DB state rather than git status so clean-tree changes
    /// from pull/checkout/merge are still reported as stale.
    pub fn get_changed_files(&self) -> Result<ChangedFiles> {
        let diff = diff_filesystem_against_index(&self.root_dir, self.queries)?;
        Ok(ChangedFiles {
            added: diff.added,
            modified: diff.modified,
            removed: diff.removed,
        })
    }
}

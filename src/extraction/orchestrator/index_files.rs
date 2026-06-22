use std::fs;

use super::parse::{extract_from_source, extraction_error_result, validate_io_path_within_root};
use super::pipeline::ExtractionOrchestrator;
use super::progress::{FileStats, IndexResult, now_ms};
use crate::error::{Result, log_warn};
use crate::extraction::grammars::{
    detect_language,
    is_file_level_only_language,
    is_language_supported,
};
use crate::types::{ExtractionError, ExtractionResult, Severity};
use crate::utils::validate_path_within_root;

impl<'a> ExtractionOrchestrator<'a> {
    /// Index specific files.
    pub fn index_files(&self, file_paths: &[String]) -> Result<IndexResult> {
        let start_time = now_ms();
        let mut errors: Vec<ExtractionError> = Vec::new();
        let mut files_indexed = 0usize;
        let mut files_skipped = 0usize;
        let mut files_errored = 0usize;
        let mut total_nodes = 0usize;
        let mut total_edges = 0usize;

        for file_path in file_paths {
            let result = self.index_file(file_path)?;

            if !result.errors.is_empty() {
                errors.extend(result.errors.iter().cloned());
            }

            if !result.nodes.is_empty() {
                files_indexed += 1;
                total_nodes += result.nodes.len();
                total_edges += result.edges.len();
            } else if result.errors.iter().any(|e| e.severity == Severity::Error) {
                files_errored += 1;
            } else {
                let tracked = self.queries.get_file_by_path(file_path)?;
                match tracked {
                    Some(t) if is_file_level_only_language(t.language) => files_indexed += 1,
                    _ => files_skipped += 1,
                }
            }
        }

        Ok(IndexResult {
            success: files_indexed > 0 || !errors.iter().any(|e| e.severity == Severity::Error),
            files_indexed,
            files_skipped,
            files_errored,
            nodes_created: total_nodes,
            edges_created: total_edges,
            errors,
            duration_ms: now_ms() - start_time,
        })
    }

    /// Index a single file.
    pub fn index_file(&self, relative_path: &str) -> Result<ExtractionResult> {
        let Ok(full_path) = validate_io_path_within_root(&self.root_dir, relative_path) else {
            return Ok(extraction_error_result(
                format!("Path traversal blocked: {relative_path}"),
                relative_path,
                "path_traversal",
            ));
        };

        // Read file content and stats
        let read = fs::metadata(&full_path).and_then(|meta| {
            let bytes = fs::read(&full_path)?;
            Ok((meta, bytes))
        });
        let (stats, content) = match read {
            Ok((meta, bytes)) => (
                FileStats::from_metadata(&meta),
                String::from_utf8_lossy(&bytes).into_owned(),
            ),
            Err(error) => {
                return Ok(extraction_error_result(
                    format!("Failed to read file: {error}"),
                    relative_path,
                    "read_error",
                ));
            }
        };

        self.index_file_with_content(relative_path, &content, &stats)
    }

    /// Index a single file with pre-read content and stats.
    /// Used by the parallel batch reader to avoid redundant file I/O.
    pub fn index_file_with_content(
        &self,
        relative_path: &str,
        content: &str,
        stats: &FileStats,
    ) -> Result<ExtractionResult> {
        // Prevent path traversal
        if validate_path_within_root(&self.root_dir, relative_path).is_none() {
            log_warn(
                "Path traversal blocked in indexFileWithContent",
                Some(&serde_json::json!({ "relativePath": relative_path })),
            );
            return Ok(extraction_error_result(
                "Path traversal blocked".to_string(),
                relative_path,
                "path_traversal",
            ));
        }

        // No size cap — files are indexed regardless of size (see the parallel
        // batch path). Exclusion is by ignore rules / generated detection, not
        // by a byte threshold.

        // Detect language
        let language = detect_language(relative_path, Some(content));
        if !is_language_supported(language) {
            return Ok(ExtractionResult::default());
        }

        // Extract from source. Use cached framework names if index_all has run,
        // otherwise detect on the spot so single-file re-index paths still emit
        // route nodes / middleware / etc.
        let framework_names = self.ensure_detected_frameworks(None);
        let result = extract_from_source(
            relative_path,
            content,
            Some(language),
            Some(&framework_names),
        );

        // Store in database
        if !result.nodes.is_empty() || result.errors.is_empty() {
            self.store_extraction_result(relative_path, content, language, stats, &result)?;
        }

        Ok(result)
    }
}

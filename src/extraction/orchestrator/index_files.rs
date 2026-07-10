use tokio_util::sync::CancellationToken;

use super::parse::{
    BatchItem,
    BatchOutcome,
    FILE_IO_BATCH_SIZE,
    extract_from_source,
    extraction_error_result,
    parse_batch,
};
use super::pipeline::ExtractionOrchestrator;
use super::progress::{FileStats, IndexResult, now_ms};
use crate::error::{CodeGraphError, Result, log_warn};
use crate::extraction::grammars::{
    detect_language_with_overrides,
    is_file_level_only_language,
    is_language_supported,
};
use crate::types::{ExtractionError, ExtractionResult, Severity};
use crate::utils::validate_path_within_root;

impl<'a> ExtractionOrchestrator<'a> {
    /// Index specific files.
    pub async fn index_files(&self, file_paths: &[String]) -> Result<IndexResult> {
        let start_time = now_ms();
        let mut errors: Vec<ExtractionError> = Vec::new();
        let mut files_indexed = 0usize;
        let mut files_skipped = 0usize;
        let mut files_errored = 0usize;
        let mut total_nodes = 0usize;
        let mut total_edges = 0usize;

        let framework_names = self.ensure_detected_frameworks(None);
        let cancellation = CancellationToken::new();
        for batch in file_paths.chunks(FILE_IO_BATCH_SIZE) {
            let items = parse_batch(
                &self.root_dir,
                batch,
                &framework_names,
                &self.project_config,
                &cancellation,
            )
            .await?;
            for item in items {
                let file_path = item.file_path.clone();
                let result = self.persist_batch_item(item)?;

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
                    let tracked = self.queries.get_file_by_path(&file_path)?;
                    match tracked {
                        Some(t) if is_file_level_only_language(t.language) => files_indexed += 1,
                        _ => files_skipped += 1,
                    }
                }
            }
        }

        Ok(IndexResult {
            success: files_indexed > 0 || !errors.iter().any(|e| e.severity == Severity::Error),
            files_indexed,
            files_skipped,
            files_errored,
            files_discovered: None,
            nodes_created: total_nodes,
            edges_created: total_edges,
            errors,
            duration_ms: now_ms() - start_time,
        })
    }

    /// Index a single file.
    pub async fn index_file(&self, relative_path: &str) -> Result<ExtractionResult> {
        let framework_names = self.ensure_detected_frameworks(None);
        let cancellation = CancellationToken::new();
        let mut items = parse_batch(
            &self.root_dir,
            &[relative_path.to_string()],
            &framework_names,
            &self.project_config,
            &cancellation,
        )
        .await?;
        self.persist_batch_item(items.pop().expect("single-file parse result"))
    }

    /// Index a single file with pre-read content and stats.
    /// Used by the parallel batch reader to avoid redundant file I/O.
    pub async fn index_file_with_content(
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
        let language = detect_language_with_overrides(
            relative_path,
            Some(content),
            self.project_config.extension_overrides(),
        );
        if !is_language_supported(language) {
            return Ok(ExtractionResult::default());
        }

        // Extract from source. Use cached framework names if index_all has run,
        // otherwise detect on the spot so single-file re-index paths still emit
        // route nodes / middleware / etc.
        let framework_names = self.ensure_detected_frameworks(None);
        let task_path = relative_path.to_string();
        let task_content = content.to_string();
        let result = tokio::task::spawn_blocking(move || {
            extract_from_source(
                &task_path,
                &task_content,
                Some(language),
                Some(&framework_names),
            )
        })
        .await
        .map_err(|error| {
            CodeGraphError::other(format!("Tokio single-file parse worker failed: {error}"))
        })?;

        // Store in database
        if !result.nodes.is_empty() || result.errors.is_empty() {
            self.store_extraction_result(relative_path, content, language, stats, &result)?;
        }

        Ok(result)
    }

    pub(super) fn persist_batch_item(&self, item: BatchItem) -> Result<ExtractionResult> {
        match item.outcome {
            BatchOutcome::ReadError(message) => {
                let (message, code) = if message == "Path traversal blocked" {
                    (message, "path_traversal")
                } else {
                    (format!("Failed to read file: {message}"), "read_error")
                };
                Ok(extraction_error_result(message, &item.file_path, code))
            }
            BatchOutcome::Parsed {
                content,
                stats,
                result,
            } => {
                let language = detect_language_with_overrides(
                    &item.file_path,
                    Some(&content),
                    self.project_config.extension_overrides(),
                );
                if !result.nodes.is_empty() || result.errors.is_empty() {
                    self.store_extraction_result(
                        &item.file_path,
                        &content,
                        language,
                        &stats,
                        &result,
                    )?;
                }
                Ok(result)
            }
        }
    }
}

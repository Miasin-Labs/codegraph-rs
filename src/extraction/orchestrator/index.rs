use std::sync::atomic::AtomicBool;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::parse::{
    BatchItem,
    BatchOutcome,
    FILE_IO_BATCH_SIZE,
    PARSE_PIPELINE_DEPTH,
    parse_batch,
};
use super::pipeline::ExtractionOrchestrator;
use super::progress::{
    IndexPhase,
    IndexProgress,
    IndexResult,
    aborted_error,
    emit,
    is_aborted,
    now_ms,
};
use super::scan::scan_directory_with_config;
use crate::error::{CodeGraphError, Result};
use crate::extraction::grammars::{
    detect_language_with_overrides,
    init_grammars,
    is_file_level_only_language,
    load_grammars_for_languages,
};
use crate::types::{ExtractionError, Language, Severity};

impl<'a> ExtractionOrchestrator<'a> {
    /// Index all files in the project.
    ///
    /// `signal`: cooperative abort flag (TS `AbortSignal`) — set to `true` to
    /// abort. `verbose` is kept for signature parity; the TS verbose logs were
    /// all worker-lifecycle messages that have no native equivalent.
    pub async fn index_all(
        &self,
        on_progress: Option<&dyn Fn(&IndexProgress)>,
        signal: Option<&AtomicBool>,
        _verbose: bool,
    ) -> Result<IndexResult> {
        init_grammars();
        let start_time = now_ms();
        let mut errors: Vec<ExtractionError> = Vec::new();
        let mut files_indexed = 0usize;
        let mut files_skipped = 0usize;
        let mut files_errored = 0usize;
        let mut total_nodes = 0usize;
        let mut total_edges = 0usize;

        // Phase 1: Scan for files
        emit(
            on_progress,
            IndexProgress {
                phase: IndexPhase::Scanning,
                current: 0,
                total: 0,
                current_file: None,
            },
        );

        let files = match on_progress {
            Some(cb) => {
                let mut scan_cb = |current: usize, file: &str| {
                    cb(&IndexProgress {
                        phase: IndexPhase::Scanning,
                        current,
                        total: 0,
                        current_file: Some(file.to_string()),
                    });
                };
                scan_directory_with_config(&self.root_dir, Some(&mut scan_cb), &self.project_config)
            }
            None => scan_directory_with_config(&self.root_dir, None, &self.project_config),
        };
        let files_discovered = files.len();

        // Detect frameworks once per index_all run using the scanned file list.
        // Names are passed to each parse call so framework-specific extractors
        // (route nodes, middleware, etc.) run after the tree-sitter pass.
        // Framework detection is reset each run so adding e.g. requirements.txt
        // between runs is picked up without restarting the process.
        *self.detected_framework_names.borrow_mut() = None;
        let framework_names = self.ensure_detected_frameworks(Some(&files));

        if is_aborted(signal) {
            return Ok(IndexResult {
                success: false,
                files_indexed: 0,
                files_skipped: 0,
                files_errored: 0,
                files_discovered: Some(files_discovered),
                nodes_created: 0,
                edges_created: 0,
                errors: vec![aborted_error()],
                duration_ms: now_ms() - start_time,
            });
        }

        // Phase 2: Parse files on Tokio's blocking pool; storage stays on this
        // thread because SQLite access is single-threaded.
        let total = files.len();
        let mut processed = 0usize;

        emit(
            on_progress,
            IndexProgress {
                phase: IndexPhase::Parsing,
                current: 0,
                total,
                current_file: None,
            },
        );

        // Detect needed languages and load grammars (no-op shims natively;
        // kept for call parity with the TS pipeline).
        let mut needed_languages: Vec<Language> = Vec::new();
        for f in &files {
            let lang =
                detect_language_with_overrides(f, None, self.project_config.extension_overrides());
            if !needed_languages.contains(&lang) {
                needed_languages.push(lang);
            }
        }
        // .h files default to 'c' but may be C++ — ensure cpp grammar is loaded when c is needed
        if needed_languages.contains(&Language::C) && !needed_languages.contains(&Language::Cpp) {
            needed_languages.push(Language::Cpp);
        }
        load_grammars_for_languages(&needed_languages);

        // Parse/store pipeline: a producer task parses batches ahead while
        // this thread stores the previous batch's results into SQLite — parsing
        // never idles behind the single-threaded store.
        // The bounded channel caps how far the producer runs ahead; dropping
        // the receiver (error/abort return paths) stops the producer after
        // its in-flight batch.
        let parse_cancel = CancellationToken::new();
        let (batch_tx, mut batch_rx) = mpsc::channel::<Vec<BatchItem>>(PARSE_PIPELINE_DEPTH);
        let producer = {
            let files = files.clone();
            let root_dir = self.root_dir.clone();
            let framework_names = framework_names.clone();
            let project_config = self.project_config.clone();
            let parse_cancel = parse_cancel.clone();
            tokio::spawn(async move {
                for batch in files.chunks(FILE_IO_BATCH_SIZE) {
                    if parse_cancel.is_cancelled() {
                        return Ok(());
                    }
                    // Read + parse the batch in parallel (with path validation
                    // before any I/O); order is preserved.
                    let items = parse_batch(
                        &root_dir,
                        batch,
                        &framework_names,
                        &project_config,
                        &parse_cancel,
                    )
                    .await?;
                    if batch_tx.send(items).await.is_err() {
                        return Ok(()); // consumer dropped the receiver
                    }
                }
                Ok::<(), CodeGraphError>(())
            })
        };

        while let Some(batch_items) = batch_rx.recv().await {
            if is_aborted(signal) {
                parse_cancel.cancel();
                drop(batch_rx);
                let _ = producer.await;
                let mut all_errors = vec![aborted_error()];
                all_errors.extend(errors);
                return Ok(IndexResult {
                    success: false,
                    files_indexed,
                    files_skipped,
                    files_errored,
                    files_discovered: Some(files_discovered),
                    nodes_created: total_nodes,
                    edges_created: total_edges,
                    errors: all_errors,
                    duration_ms: now_ms() - start_time,
                });
            }

            // Store results on this thread (SQLite is not thread-safe).
            for item in batch_items {
                if is_aborted(signal) {
                    parse_cancel.cancel();
                    drop(batch_rx);
                    let _ = producer.await;
                    let mut all_errors = vec![aborted_error()];
                    all_errors.extend(errors);
                    return Ok(IndexResult {
                        success: false,
                        files_indexed,
                        files_skipped,
                        files_errored,
                        files_discovered: Some(files_discovered),
                        nodes_created: total_nodes,
                        edges_created: total_edges,
                        errors: all_errors,
                        duration_ms: now_ms() - start_time,
                    });
                }

                // Report progress before handling (show current file being worked on)
                emit(
                    on_progress,
                    IndexProgress {
                        phase: IndexPhase::Parsing,
                        current: processed,
                        total,
                        current_file: Some(item.file_path.clone()),
                    },
                );

                match item.outcome {
                    BatchOutcome::ReadError(message) => {
                        processed += 1;
                        files_errored += 1;
                        errors.push(ExtractionError {
                            message: format!("Failed to read file: {message}"),
                            file_path: Some(item.file_path),
                            line: None,
                            column: None,
                            severity: Severity::Error,
                            code: Some("read_error".to_string()),
                        });
                    }
                    BatchOutcome::Parsed {
                        content,
                        stats,
                        mut result,
                    } => {
                        processed += 1;

                        // Store in database (errors stored on the file record are
                        // pre-filePath-fill, matching the TS serialization order).
                        if !result.nodes.is_empty() || result.errors.is_empty() {
                            let language = detect_language_with_overrides(
                                &item.file_path,
                                Some(&content),
                                self.project_config.extension_overrides(),
                            );
                            if let Err(error) = self.store_extraction_result(
                                &item.file_path,
                                &content,
                                language,
                                &stats,
                                &result,
                            ) {
                                parse_cancel.cancel();
                                drop(batch_rx);
                                let _ = producer.await;
                                return Err(error);
                            }
                        }

                        if !result.errors.is_empty() {
                            for err in result.errors.iter_mut() {
                                if err.file_path.is_none() {
                                    err.file_path = Some(item.file_path.clone());
                                }
                            }
                            errors.extend(result.errors.iter().cloned());
                        }

                        if !result.nodes.is_empty() {
                            files_indexed += 1;
                            total_nodes += result.nodes.len();
                            total_edges += result.edges.len();
                        } else if result.errors.iter().any(|e| e.severity == Severity::Error) {
                            files_errored += 1;
                        } else {
                            // Files with no symbols but no errors (yaml, twig, properties) are
                            // tracked at the file level — count them as indexed so the CLI
                            // doesn't misleadingly report "No files found to index".
                            let lang = detect_language_with_overrides(
                                &item.file_path,
                                Some(&content),
                                self.project_config.extension_overrides(),
                            );
                            if is_file_level_only_language(lang) {
                                files_indexed += 1;
                            } else {
                                files_skipped += 1;
                            }
                        }
                    }
                }
            }
        }

        match producer.await {
            Ok(result) => result?,
            Err(error) => {
                return Err(CodeGraphError::other(format!(
                    "Tokio parse producer failed: {error}"
                )));
            }
        }

        // Report 100% so the progress bar doesn't hang at 99%
        emit(
            on_progress,
            IndexProgress {
                phase: IndexPhase::Parsing,
                current: total,
                total,
                current_file: None,
            },
        );

        // (The TS WASM memory-error retry pass is N/A natively and was dropped.)

        Ok(IndexResult {
            success: files_indexed > 0 || !errors.iter().any(|e| e.severity == Severity::Error),
            files_indexed,
            files_skipped,
            files_errored,
            files_discovered: Some(files_discovered),
            nodes_created: total_nodes,
            edges_created: total_edges,
            errors,
            duration_ms: now_ms() - start_time,
        })
    }
}

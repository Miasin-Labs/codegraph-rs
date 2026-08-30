use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{fmt, fs};

use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::progress::FileStats;
use crate::error::{CodeGraphError, Result, log_warn};
use crate::extraction::astro_extractor::AstroExtractor;
use crate::extraction::cfml_extractor::CfmlExtractor;
use crate::extraction::dfm_extractor::DfmExtractor;
use crate::extraction::grammars::{
    detect_language,
    detect_language_with_overrides,
    is_file_level_only_language,
};
use crate::extraction::ida_c_extractor::{IdaCExtractor, is_ida_generated_c};
use crate::extraction::languages;
use crate::extraction::liquid_extractor::LiquidExtractor;
use crate::extraction::lwc_template::LwcTemplateExtractor;
use crate::extraction::mybatis_extractor::MyBatisExtractor;
use crate::extraction::razor_extractor::RazorExtractor;
use crate::extraction::salesforce_markup::SalesforceMarkupExtractor;
use crate::extraction::svelte_extractor::SvelteExtractor;
use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
use crate::extraction::unlace_extractor::{self, UnlaceCExtractor};
use crate::extraction::vue_extractor::VueExtractor;
use crate::project_config::ProjectConfig;
use crate::resolution::frameworks::{get_all_framework_resolvers, get_applicable_frameworks};
use crate::types::{ExtractionError, ExtractionResult, Language, Severity, UnresolvedReference};
use crate::utils::validate_path_within_root;

/// Number of files to read + parse in parallel per batch during indexing.
/// Each batch fans out across Tokio's blocking pool, then results are stored
/// serially (SQLite is single-threaded), so the batch size caps effective
/// parallelism and amortizes the store barrier. Worst-case memory is
/// `FILE_IO_BATCH_SIZE` files of held content (plus extraction results). Files
/// larger than `MAX_FILE_SIZE` are skipped before their body is read, so a
/// batch's footprint is bounded by the cap times the batch size.
pub(super) const FILE_IO_BATCH_SIZE: usize = 64;

/// How many fully parsed batches the parse producer may run ahead of the
/// (single-threaded) store loop. Bounds peak memory to
/// `(PARSE_PIPELINE_DEPTH + 1) × FILE_IO_BATCH_SIZE` files of content +
/// extraction results while keeping parse workers busy during SQLite writes.
pub(super) const PARSE_PIPELINE_DEPTH: usize = 2;

/// Default maximum size of a file that gets parsed. Files larger than this are
/// recorded with a `size_exceeded` warning and skipped, mirroring the TS
/// reference (`src/extraction/index.ts` `MAX_FILE_SIZE`). Vendored/generated
/// multi-MB inputs — minified bundles, amalgamated `sqlite3.c`, base64 resource
/// blobs like `qrc_emoji_*.cpp` — carry no useful symbols but do carry
/// pathological syntax trees (tens of thousands of siblings under one node)
/// that make extraction quadratic. Skipping them by size is the cheap,
/// deterministic guard the parser-level algorithmic fixes back up. 1 MiB
/// covers essentially all hand-written source.
pub(super) const DEFAULT_MAX_FILE_SIZE: u64 = 1024 * 1024;

/// Resolved per-file size cap. Overridable with `CODEGRAPH_MAX_FILE_SIZE`
/// (bytes); a value of `0` disables the cap entirely (index everything, at the
/// caller's own risk on pathological inputs). Invalid/empty values fall back to
/// [`DEFAULT_MAX_FILE_SIZE`].
pub(super) fn max_file_size() -> u64 {
    match std::env::var("CODEGRAPH_MAX_FILE_SIZE") {
        Ok(raw) if !raw.trim().is_empty() => {
            raw.trim().parse::<u64>().unwrap_or(DEFAULT_MAX_FILE_SIZE)
        }
        _ => DEFAULT_MAX_FILE_SIZE,
    }
}

pub(super) fn worker_count_for(work_items: usize) -> usize {
    let available = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    work_items.clamp(1, available)
}

// =============================================================================
// extractFromSource dispatcher (bottom of src/extraction/tree-sitter.ts)
// =============================================================================

pub(super) fn unresolved_ref_to_reference(
    r: crate::resolution::types::UnresolvedRef,
) -> UnresolvedReference {
    UnresolvedReference {
        from_node_id: r.from_node_id,
        reference_name: r.reference_name,
        reference_kind: r.reference_kind,
        line: r.line,
        column: r.column,
        file_path: Some(r.file_path),
        language: Some(r.language),
        candidates: r.candidates,
        metadata: r.metadata,
    }
}

/// Extract nodes and edges from source code.
///
/// If `framework_names` is provided, framework-specific extractors matching
/// those names and the file's language are run after the tree-sitter pass.
/// Their nodes/references/errors are merged into the returned result.
pub fn extract_from_source(
    file_path: &str,
    source: &str,
    language: Option<Language>,
    framework_names: Option<&[String]>,
) -> ExtractionResult {
    let detected_language = language.unwrap_or_else(|| detect_language(file_path, Some(source)));
    let file_extension = Path::new(file_path)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default();

    // Whole-program decompiled C: one `.c` per binary, either unlace's
    // `/* Function: */` blocks or IDA `decompile_many()`'s `//----- (addr) -----`
    // boundaries. Split into per-function units (checked before the IDA
    // single-function path, which these aggregate files would otherwise
    // truncate to their first function).
    let mut result = if (detected_language == Language::C || detected_language == Language::Cpp)
        && (unlace_extractor::is_unlace_c(file_path, source)
            || unlace_extractor::is_ida_decompile_many(source))
    {
        UnlaceCExtractor::new(file_path, source, detected_language).extract()
    } else if (detected_language == Language::C || detected_language == Language::Cpp)
        // IDA/Hex-Rays decompiler output is C-like but often not valid C
        // (`.name` thunk symbols, IDA typedefs, huge one-function dumps).
        && is_ida_generated_c(file_path, source)
    {
        IdaCExtractor::new(file_path, source, detected_language).extract()
    } else if detected_language == Language::Svelte {
        // Use custom extractor for Svelte
        SvelteExtractor::new(file_path, source, languages::extractor_for).extract()
    } else if detected_language == Language::Vue {
        // Use custom extractor for Vue
        VueExtractor::new(file_path, source, languages::extractor_for).extract()
    } else if detected_language == Language::Astro {
        AstroExtractor::new(file_path, source).extract()
    } else if detected_language == Language::Razor {
        RazorExtractor::new(file_path, source).extract()
    } else if matches!(detected_language, Language::Cfml | Language::Cfscript) {
        CfmlExtractor::new(file_path, source, detected_language).extract()
    } else if detected_language == Language::Liquid {
        // Use custom extractor for Liquid
        LiquidExtractor::new(file_path, source).extract()
    } else if detected_language == Language::Xml {
        // Custom extractor for MyBatis mapper XML. Non-mapper XML returns just a
        // file node so the watcher tracks it without emitting symbols.
        MyBatisExtractor::new(file_path, source).extract()
    } else if detected_language == Language::Html {
        // HTML: file node always; LWC templates additionally emit `{binding}`
        // references to their component JS class members.
        LwcTemplateExtractor::new(file_path, source).extract()
    } else if matches!(detected_language, Language::Visualforce | Language::Aura) {
        // Salesforce markup: controller/extensions attributes and `{!...}`
        // bindings become Apex / client-controller references.
        SalesforceMarkupExtractor::new(file_path, source, detected_language).extract()
    } else if is_file_level_only_language(detected_language) {
        // No symbol extraction at this stage — files are tracked at the file-record
        // level only. Framework extractors (Drupal routing yml, Spring `@Value`
        // resolution against application.yml/application.properties) run later and
        // add per-file nodes/references when they apply.
        ExtractionResult::default()
    } else if detected_language == Language::Pascal
        && (file_extension == ".dfm" || file_extension == ".fmx")
    {
        // Use custom extractor for DFM/FMX form files
        DfmExtractor::new(file_path, source).extract()
    } else {
        TreeSitterExtractor::new(
            file_path,
            source,
            Some(detected_language),
            languages::extractor_for(detected_language),
        )
        .extract()
    };

    // Framework-specific extraction (routes, middleware, etc.)
    if let Some(names) = framework_names {
        if !names.is_empty() {
            let matching: Vec<_> = get_all_framework_resolvers()
                .into_iter()
                .filter(|r| names.iter().any(|n| n == r.name()))
                .collect();
            let applicable = get_applicable_frameworks(&matching, detected_language);
            for fw in applicable {
                // TS wraps fw.extract in try/catch pushing a
                // `Framework extractor '{name}' failed: {err}` warning; the Rust
                // extract hooks are infallible, so the catch arm is unreachable
                // and was dropped (documented in notes).
                if let Some(fw_result) = fw.extract(file_path, source) {
                    result.nodes.extend(fw_result.nodes);
                    result.unresolved_references.extend(
                        fw_result
                            .references
                            .into_iter()
                            .map(unresolved_ref_to_reference),
                    );
                }
            }
        }
    }

    result
}

// =============================================================================
// ExtractionOrchestrator
// =============================================================================

#[derive(Debug)]
pub(super) enum ReadFailure {
    Cancelled,
    PathTraversal,
    NotRegularFile,
    Io(std::io::Error),
}

impl ReadFailure {
    pub(super) fn code(&self) -> &'static str {
        match self {
            Self::PathTraversal => "path_traversal",
            Self::Cancelled | Self::NotRegularFile | Self::Io(_) => "read_error",
        }
    }

    pub(super) fn extraction_message(&self) -> String {
        match self {
            Self::PathTraversal => self.to_string(),
            Self::Cancelled | Self::NotRegularFile | Self::Io(_) => {
                format!("Failed to read file: {self}")
            }
        }
    }
}

impl fmt::Display for ReadFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("Parsing cancelled"),
            Self::PathTraversal => formatter.write_str("Path traversal blocked"),
            Self::NotRegularFile => formatter.write_str("Path is not a regular file"),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ReadFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Cancelled | Self::PathTraversal | Self::NotRegularFile => None,
        }
    }
}

/// Outcome of the parallel read+parse stage for one file.
pub(super) enum BatchOutcome {
    ReadError(ReadFailure),
    Parsed {
        content: String,
        stats: FileStats,
        result: ExtractionResult,
    },
}

pub(super) struct BatchItem {
    pub(super) file_path: String,
    pub(super) outcome: BatchOutcome,
}

pub(super) async fn parse_batch(
    root_dir: &Path,
    batch: &[String],
    framework_names: &[String],
    project_config: &ProjectConfig,
    cancellation: &CancellationToken,
) -> Result<Vec<BatchItem>> {
    let total = batch.len();
    if total == 0 {
        return Ok(Vec::new());
    }

    let root_dir = Arc::new(root_dir.to_path_buf());
    let framework_names = Arc::new(framework_names.to_vec());
    let project_config = Arc::new(project_config.clone());
    let mut slots: Vec<Option<BatchItem>> = (0..total).map(|_| None).collect();
    let mut workers = JoinSet::new();
    let worker_limit = worker_count_for(total);
    let mut next = 0usize;

    while next < total || !workers.is_empty() {
        while next < total && workers.len() < worker_limit && !cancellation.is_cancelled() {
            let index = next;
            let file_path = batch[index].clone();
            let root_dir = Arc::clone(&root_dir);
            let framework_names = Arc::clone(&framework_names);
            let project_config = Arc::clone(&project_config);
            let cancellation = cancellation.clone();
            workers.spawn_blocking(move || {
                let item = if cancellation.is_cancelled() {
                    BatchItem {
                        file_path,
                        outcome: BatchOutcome::ReadError(ReadFailure::Cancelled),
                    }
                } else {
                    read_and_parse(
                        root_dir.as_path(),
                        &file_path,
                        framework_names.as_slice(),
                        project_config.as_ref(),
                    )
                };
                (index, item)
            });
            next += 1;
        }

        if cancellation.is_cancelled() {
            while workers.join_next().await.is_some() {}
            return Err(CodeGraphError::other("parsing cancelled"));
        }

        match workers.join_next().await {
            Some(Ok((index, item))) => slots[index] = Some(item),
            Some(Err(error)) => {
                cancellation.cancel();
                while workers.join_next().await.is_some() {}
                return Err(CodeGraphError::other(format!(
                    "Tokio parse worker failed: {error}"
                )));
            }
            None => break,
        }
    }

    slots
        .into_iter()
        .enumerate()
        .map(|(index, item)| {
            item.ok_or_else(|| {
                CodeGraphError::other(format!("parse worker did not return result {index}"))
            })
        })
        .collect()
}

pub(super) fn validate_io_path_within_root(
    root_dir: &Path,
    file_path: &str,
) -> std::result::Result<PathBuf, ()> {
    let full_path = validate_path_within_root(root_dir, file_path).ok_or(())?;
    let real_root = match fs::canonicalize(root_dir) {
        Ok(path) => path,
        Err(_) => return Ok(full_path),
    };

    match fs::canonicalize(&full_path) {
        Ok(real_path) => {
            if real_path == real_root || real_path.starts_with(&real_root) {
                Ok(full_path)
            } else {
                Err(())
            }
        }
        Err(_) => {
            if fs::symlink_metadata(&full_path)
                .map(|meta| meta.file_type().is_symlink())
                .unwrap_or(false)
            {
                Err(())
            } else {
                Ok(full_path)
            }
        }
    }
}

/// Read + size-check + parse a single file. Runs on Tokio's blocking pool, so it
/// must not touch the orchestrator (the DB handle is not `Sync`).
pub(super) fn read_and_parse(
    root_dir: &Path,
    file_path: &str,
    framework_names: &[String],
    project_config: &ProjectConfig,
) -> BatchItem {
    let Ok(full_path) = validate_io_path_within_root(root_dir, file_path) else {
        log_warn(
            "Path traversal blocked in batch reader",
            Some(&serde_json::json!({ "filePath": file_path })),
        );
        return BatchItem {
            file_path: file_path.to_string(),
            outcome: BatchOutcome::ReadError(ReadFailure::PathTraversal),
        };
    };

    let meta = match fs::metadata(&full_path) {
        Ok(meta) if meta.is_file() => meta,
        Ok(_) => {
            return BatchItem {
                file_path: file_path.to_string(),
                outcome: BatchOutcome::ReadError(ReadFailure::NotRegularFile),
            };
        }
        Err(err) => {
            return BatchItem {
                file_path: file_path.to_string(),
                outcome: BatchOutcome::ReadError(ReadFailure::Io(err)),
            };
        }
    };

    // Honour MAX_FILE_SIZE before reading the body. Vendored/generated multi-MB
    // files (minified bundles, amalgamated C, base64 resource blobs) carry no
    // useful symbols but do carry pathological syntax trees that make
    // extraction quadratic; skip them with a `size_exceeded` warning (TS parity)
    // and still record the file so accounting/reconciliation stays correct.
    let size_cap = max_file_size();
    if size_cap > 0 && meta.len() > size_cap {
        return BatchItem {
            file_path: file_path.to_string(),
            outcome: BatchOutcome::Parsed {
                content: String::new(),
                stats: FileStats::from_metadata(&meta),
                result: ExtractionResult {
                    errors: vec![ExtractionError {
                        message: format!(
                            "File exceeds max size ({} > {})",
                            meta.len(),
                            size_cap
                        ),
                        file_path: Some(file_path.to_string()),
                        line: None,
                        column: None,
                        severity: Severity::Warning,
                        code: Some("size_exceeded".to_string()),
                    }],
                    ..Default::default()
                },
            },
        };
    }

    let bytes = match fs::read(&full_path) {
        Ok(bytes) => bytes,
        Err(err) => {
            return BatchItem {
                file_path: file_path.to_string(),
                outcome: BatchOutcome::ReadError(ReadFailure::Io(err)),
            };
        }
    };
    let content = String::from_utf8_lossy(&bytes).into_owned();
    let stats = FileStats::from_metadata(&meta);

    let language = detect_language_with_overrides(
        file_path,
        Some(&content),
        project_config.extension_overrides(),
    );
    let result = extract_from_source(file_path, &content, Some(language), Some(framework_names));
    BatchItem {
        file_path: file_path.to_string(),
        outcome: BatchOutcome::Parsed {
            content,
            stats,
            result,
        },
    }
}

pub(super) fn extraction_error_result(
    message: String,
    file_path: &str,
    code: &str,
) -> ExtractionResult {
    extraction_error_result_with_severity(message, file_path, code, Severity::Error)
}

/// Like [`extraction_error_result`] but with an explicit severity. Used for the
/// `size_exceeded` skip, which is a warning (the file is intentionally not
/// parsed) rather than an error.
pub(super) fn extraction_error_result_with_severity(
    message: String,
    file_path: &str,
    code: &str,
    severity: Severity,
) -> ExtractionResult {
    ExtractionResult {
        errors: vec![ExtractionError {
            message,
            file_path: Some(file_path.to_string()),
            line: None,
            column: None,
            severity,
            code: Some(code.to_string()),
        }],
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn parse_batch_preserves_input_order() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("first.rs"), "fn first() {}\n").unwrap();
        fs::write(temp.path().join("second.rs"), "fn second() {}\n").unwrap();
        let batch = vec!["second.rs".to_string(), "first.rs".to_string()];

        let parsed = parse_batch(
            temp.path(),
            &batch,
            &[],
            &ProjectConfig::default(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(
            parsed
                .iter()
                .map(|item| item.file_path.as_str())
                .collect::<Vec<_>>(),
            ["second.rs", "first.rs"]
        );
        assert!(
            parsed
                .iter()
                .all(|item| matches!(item.outcome, BatchOutcome::Parsed { .. }))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn parse_batch_observes_preexisting_cancellation() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let result = parse_batch(
            Path::new("/missing"),
            &["never-read.rs".to_string()],
            &[],
            &ProjectConfig::default(),
            &cancellation,
        )
        .await;

        match result {
            Err(error) => assert!(error.to_string().contains("parsing cancelled")),
            Ok(_) => panic!("cancelled parse unexpectedly succeeded"),
        }
    }

    #[test]
    fn read_and_parse_rejects_directory_paths_before_reading() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("not_a_file.rs")).unwrap();

        let item = read_and_parse(temp.path(), "not_a_file.rs", &[], &ProjectConfig::default());

        match item.outcome {
            BatchOutcome::ReadError(failure) => {
                assert!(matches!(failure, ReadFailure::NotRegularFile));
                assert_eq!(failure.code(), "read_error");
                assert_eq!(failure.to_string(), "Path is not a regular file");
            }
            BatchOutcome::Parsed { .. } => panic!("directory path unexpectedly parsed"),
        }
    }

    #[test]
    fn read_and_parse_skips_files_over_max_size_with_warning() {
        let temp = tempfile::tempdir().unwrap();
        // Write a file larger than MAX_FILE_SIZE.
        let big = "a".repeat((max_file_size() as usize) + 1024);
        std::fs::write(temp.path().join("huge.cpp"), &big).unwrap();

        let item = read_and_parse(temp.path(), "huge.cpp", &[], &ProjectConfig::default());

        match item.outcome {
            BatchOutcome::Parsed {
                content, result, ..
            } => {
                // Body is not read/parsed; a size_exceeded warning is recorded.
                assert!(content.is_empty(), "oversized file body should not be read");
                assert!(result.nodes.is_empty());
                assert_eq!(result.errors.len(), 1);
                let err = &result.errors[0];
                assert_eq!(err.code.as_deref(), Some("size_exceeded"));
                assert_eq!(err.severity, Severity::Warning);
            }
            BatchOutcome::ReadError(failure) => {
                panic!("oversized file should be a size_exceeded warning, got {failure:?}")
            }
        }
    }

    #[test]
    fn read_and_parse_indexes_files_at_or_below_max_size() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("ok.rs"), "fn small() {}
").unwrap();

        let item = read_and_parse(temp.path(), "ok.rs", &[], &ProjectConfig::default());

        match item.outcome {
            BatchOutcome::Parsed { content, .. } => {
                assert!(content.contains("fn small"));
            }
            BatchOutcome::ReadError(failure) => panic!("small file failed: {failure:?}"),
        }
    }

}

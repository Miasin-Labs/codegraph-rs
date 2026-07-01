use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ws_deque::scheduler;

use super::progress::FileStats;
use crate::error::log_warn;
use crate::extraction::dfm_extractor::DfmExtractor;
use crate::extraction::grammars::{detect_language, is_file_level_only_language};
use crate::extraction::ida_c_extractor::{IdaCExtractor, is_ida_generated_c};
use crate::extraction::languages;
use crate::extraction::liquid_extractor::LiquidExtractor;
use crate::extraction::lwc_template::LwcTemplateExtractor;
use crate::extraction::mybatis_extractor::MyBatisExtractor;
use crate::extraction::salesforce_markup::SalesforceMarkupExtractor;
use crate::extraction::svelte_extractor::SvelteExtractor;
use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
use crate::extraction::unlace_extractor::{self, UnlaceCExtractor};
use crate::extraction::vue_extractor::VueExtractor;
use crate::resolution::frameworks::{get_all_framework_resolvers, get_applicable_frameworks};
use crate::types::{ExtractionError, ExtractionResult, Language, Severity, UnresolvedReference};
use crate::utils::validate_path_within_root;

/// Number of files to read + parse in parallel per batch during indexing.
/// Each batch fans out across the work-stealing scheduler, then results are stored
/// serially (SQLite is single-threaded), so the batch size caps effective
/// parallelism and amortizes the store barrier. Worst-case memory is
/// `FILE_IO_BATCH_SIZE` files of held content (plus extraction results) — there
/// is no per-file size cap, so a batch's footprint scales with its largest
/// files.
pub(super) const FILE_IO_BATCH_SIZE: usize = 64;

/// How many fully parsed batches the parse producer may run ahead of the
/// (single-threaded) store loop. Bounds peak memory to
/// `(PARSE_PIPELINE_DEPTH + 1) × FILE_IO_BATCH_SIZE` files of content +
/// extraction results while keeping parse workers busy during SQLite writes.
pub(super) const PARSE_PIPELINE_DEPTH: usize = 2;

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

/// Outcome of the parallel read+parse stage for one file.
pub(super) enum BatchOutcome {
    ReadError(String),
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

pub(super) fn parse_batch(
    root_dir: &Path,
    batch: &[String],
    framework_names: &[String],
) -> Vec<BatchItem> {
    if batch.len() <= 1 {
        return batch
            .iter()
            .map(|fp| read_and_parse(root_dir, fp, framework_names))
            .collect();
    }

    let slots: Vec<Mutex<Option<BatchItem>>> = (0..batch.len()).map(|_| Mutex::new(None)).collect();
    scheduler::run(
        worker_count_for(batch.len()),
        batch.iter().enumerate(),
        |(index, fp), _| {
            let item = read_and_parse(root_dir, fp, framework_names);
            let mut slot = slots[index]
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *slot = Some(item);
        },
    );

    slots
        .into_iter()
        .map(|slot| match slot.into_inner() {
            Ok(Some(item)) => item,
            Ok(None) => panic!("parallel parse worker did not write a result"),
            Err(poisoned) => poisoned
                .into_inner()
                .unwrap_or_else(|| panic!("parallel parse worker did not write a result")),
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

/// Read + size-check + parse a single file. Runs on scheduler worker threads, so
/// it must not touch the orchestrator (the DB handle is not `Sync`).
pub(super) fn read_and_parse(
    root_dir: &Path,
    file_path: &str,
    framework_names: &[String],
) -> BatchItem {
    let Ok(full_path) = validate_io_path_within_root(root_dir, file_path) else {
        log_warn(
            "Path traversal blocked in batch reader",
            Some(&serde_json::json!({ "filePath": file_path })),
        );
        return BatchItem {
            file_path: file_path.to_string(),
            outcome: BatchOutcome::ReadError("Path traversal blocked".to_string()),
        };
    };

    let read = fs::read(&full_path).and_then(|bytes| {
        let meta = fs::metadata(&full_path)?;
        Ok((bytes, meta))
    });
    let (content, stats) = match read {
        Ok((bytes, meta)) => (
            String::from_utf8_lossy(&bytes).into_owned(),
            FileStats::from_metadata(&meta),
        ),
        Err(err) => {
            return BatchItem {
                file_path: file_path.to_string(),
                outcome: BatchOutcome::ReadError(err.to_string()),
            };
        }
    };

    // No size cap: every file is indexed regardless of size (large
    // hand-written sources — e.g. a 1.2 MiB expression printer — must not be
    // silently dropped). Pathological inputs are excluded by .gitignore /
    // ignore-dir / generated-file detection, not by a byte threshold.
    let language = detect_language(file_path, Some(&content));
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
    ExtractionResult {
        errors: vec![ExtractionError {
            message,
            file_path: Some(file_path.to_string()),
            line: None,
            column: None,
            severity: Severity::Error,
            code: Some(code.to_string()),
        }],
        ..Default::default()
    }
}

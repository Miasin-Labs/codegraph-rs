//! Extraction Orchestrator
//!
//! Coordinates file scanning, parsing, and database storage.
//!
//! Ported from `src/extraction/index.ts`, plus the `extractFromSource`
//! dispatcher from the bottom of `src/extraction/tree-sitter.ts` (deferred to
//! this file by the extraction-core port because the standalone extractors it
//! routes to were concurrent stubs at that time — see
//! `notes/extraction-core.md`).
//!
//! Node-isms dropped (documented in `notes/extraction-orchestrator.md`):
//! - `parse-worker.ts` and the whole worker lifecycle (spawn/recycle/timeout,
//!   `PARSE_TIMEOUT_MS`, `WORKER_RECYCLE_INTERVAL`) — parsing is native and
//!   in-process; parallelism is work-stealing over read batches instead.
//! - The WASM memory-corruption retry pass (fresh-worker retry + comment
//!   stripping) — there is no WASM heap to corrupt.
//! - `scanDirectoryAsync` — only existed to yield to the Node event loop;
//!   [`scan_directory`] covers both call sites.

use std::cell::RefCell;
use std::path::PathBuf;

use crate::db::QueryBuilder;

/// Extraction orchestrator.
pub struct ExtractionOrchestrator<'a> {
    pub(super) root_dir: PathBuf,
    pub(super) queries: &'a QueryBuilder,
    /// Names of frameworks detected for this project, populated by `index_all()`.
    /// Passed to `extract_from_source` so framework-specific extractors (route
    /// nodes, middleware, etc.) run after the tree-sitter pass. Cleared if
    /// detection hasn't run yet so single-file re-index paths can detect on
    /// the spot. (`RefCell`: the TS class mutates this through `&self`-shaped
    /// call paths.)
    pub(super) detected_framework_names: RefCell<Option<Vec<String>>>,
}

impl<'a> ExtractionOrchestrator<'a> {
    pub fn new(root_dir: impl Into<PathBuf>, queries: &'a QueryBuilder) -> Self {
        ExtractionOrchestrator {
            root_dir: root_dir.into(),
            queries,
            detected_framework_names: RefCell::new(None),
        }
    }
}

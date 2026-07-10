//! CodeGraph
//!
//! A local-first code intelligence system that builds a semantic
//! knowledge graph from any codebase.
//!
//! Port of `src/index.ts` — the `CodeGraph` facade that wires every layer:
//! extraction → resolution → graph → context, plus lifecycle (init/open/close),
//! cross-process file locking, and the auto-sync file watcher.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tokio::sync::{Mutex, MutexGuard};

use crate::context::{ContextBuilder, create_context_builder};
// =============================================================================
// Re-exports for consumers (mirrors the `export { … }` block of src/index.ts;
// lib.rs surfaces these at the crate root via `pub use codegraph::*`).
// `export * from './types'` and the core error types are already re-exported
// by lib.rs.
// =============================================================================

// Storage building blocks for embedded/SDK consumers that drive the graph
// directly (open a DB, run prepared queries) rather than through the CodeGraph
// facade (issue #354). RoutingManifest/TopRouteFile/SqliteBackend appear in
// facade method signatures, so they're surfaced too.
pub use crate::db::{
    DatabaseConnection,
    QueryBuilder,
    RoutingManifest,
    SqliteBackend,
    TopRouteFile,
    get_database_path,
};
pub use crate::directory::{
    CODEGRAPH_DIR,
    codegraph_dir_name,
    find_nearest_codegraph_root,
    get_codegraph_dir,
    is_codegraph_data_dir,
    is_initialized,
};
use crate::directory::{create_directory, remove_directory, validate_directory};
use crate::error::{CodeGraphError, Result};
pub use crate::error::{DefaultLogger, set_logger};
pub use crate::extraction::{
    ChangedFiles,
    EXTRACTION_VERSION,
    IndexProgress,
    IndexResult,
    SyncResult,
    detect_language,
    get_supported_languages,
    init_grammars,
    is_grammar_loaded,
    is_language_supported,
    load_all_grammars,
    load_grammars_for_languages,
};
use crate::extraction::{ExtractionOrchestrator, IndexPhase, extract_from_source};
use crate::graph::{GraphQueryManager, GraphTraverser};
pub use crate::graph::{NodeMetrics, PathStep};
// TS `export { MCPServer } from './mcp'` — added by the MCP-server wave (the
// codegraph-api notes left this re-export to the MCP owner).
pub use crate::mcp::MCPServer;
pub use crate::resolution::ResolutionResult;
use crate::resolution::{ReferenceResolver, create_resolver};
use crate::search::split_identifier_segments;
use crate::sync::{DEFAULT_READY_TIMEOUT_MS, SyncError, SyncFn, WatchSyncResult};
pub use crate::sync::{FileWatcher, LockUnavailableError, PendingFile, WatchOptions};
use crate::types::{
    BuildContextOptions,
    Context,
    Edge,
    EdgeKind,
    ExtractionError,
    ExtractionResult,
    FileRecord,
    FindRelevantContextOptions,
    GraphStats,
    Node,
    NodeKind,
    NodeRef,
    SearchOptions,
    SearchResult,
    Severity,
    Subgraph,
    TaskContext,
    TaskInput,
    TraversalOptions,
    UnresolvedReference,
};
pub use crate::utils::FileLock;

/// A graph-verified symbol matched from prose through identifier segments.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SegmentMatch {
    pub name: String,
    pub kind: NodeKind,
    pub file_path: String,
    pub start_line: u32,
    pub matched_words: Vec<String>,
}

// =============================================================================
// Options
// =============================================================================

/// Options for initializing a new CodeGraph project.
#[derive(Clone, Copy, Default)]
pub struct InitOptions<'a> {
    /// Progress callback for indexing
    pub on_progress: Option<&'a dyn Fn(&IndexProgress)>,
}

/// Options for opening an existing CodeGraph project.
#[derive(Clone, Copy, Default)]
pub struct OpenOptions {
    /// Whether to run sync if files have changed
    pub sync: bool,
    /// Whether to run in read-only mode (declared in TS but unused there too)
    pub read_only: bool,
}

/// Options for indexing.
#[derive(Clone, Copy, Default)]
pub struct IndexOptions<'a> {
    /// Progress callback
    pub on_progress: Option<&'a dyn Fn(&IndexProgress)>,
    /// Abort flag for cancellation (TS `AbortSignal` → `true` = aborted)
    pub signal: Option<&'a AtomicBool>,
    /// Enable verbose logging (worker lifecycle, memory, timeouts)
    pub verbose: bool,
}

/// Completeness of the most recent full-index run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexState {
    Indexing,
    Complete,
    Partial,
    Failed,
}

impl IndexState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Indexing => "indexing",
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "indexing" => Some(Self::Indexing),
            "complete" => Some(Self::Complete),
            "partial" => Some(Self::Partial),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// Engine versions recorded by the last successful, non-empty full index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexBuildInfo {
    pub version: Option<String>,
    pub extraction_version: Option<u32>,
}

// =============================================================================
// Helpers
// =============================================================================

/// `path.resolve(projectRoot)` parity: resolve against the current working
/// directory, collapsing `.`/`..` lexically.
fn resolve_root(project_root: &Path) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    crate::utils::lexical_resolve(&cwd, &project_root.to_string_lossy())
}

/// The exact TS lock-failure result shape `indexAll`/`indexFiles` return when
/// the cross-process file lock cannot be acquired.
fn lock_failure_index_result() -> IndexResult {
    IndexResult {
        success: false,
        files_indexed: 0,
        files_skipped: 0,
        files_errored: 0,
        files_discovered: None,
        nodes_created: 0,
        edges_created: 0,
        errors: vec![ExtractionError {
            message: "Could not acquire file lock - another process may be indexing".to_string(),
            file_path: None,
            line: None,
            column: None,
            severity: Severity::Error,
            code: None,
        }],
        duration_ms: 0,
    }
}

/// The exact TS zero-shape `sync()` returns when the file lock cannot be
/// acquired. The watcher detects this shape to keep pending files (#449):
/// a real empty sync always has `files_checked > 0`.
fn lock_failure_sync_result() -> SyncResult {
    SyncResult {
        files_checked: 0,
        files_added: 0,
        files_modified: 0,
        files_removed: 0,
        nodes_updated: 0,
        duration_ms: 0,
        changed_file_paths: None,
        changed_node_names: None,
    }
}

/// Emit a `resolving`-phase progress event (TS `{ phase: 'resolving', current, total }`).
fn emit_resolving(on_progress: Option<&dyn Fn(&IndexProgress)>, current: usize, total: usize) {
    if let Some(cb) = on_progress {
        cb(&IndexProgress {
            phase: IndexPhase::Resolving,
            current,
            total,
            current_file: None,
        });
    }
}

/// TS `new Map([...byFile, ...byName].map((ref) => [key, ref])).values()`:
/// dedupe by `fromNodeId\0referenceName\0referenceKind`, keeping the FIRST
/// occurrence's position but the LAST occurrence's value.
fn dedupe_unresolved_refs(
    by_file: Vec<UnresolvedReference>,
    by_name: Vec<UnresolvedReference>,
) -> Vec<UnresolvedReference> {
    let mut order: Vec<UnresolvedReference> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for r in by_file.into_iter().chain(by_name) {
        let key = format!(
            "{}\0{}\0{}",
            r.from_node_id,
            r.reference_name,
            r.reference_kind.as_str()
        );
        match index.get(&key) {
            Some(&i) => order[i] = r,
            None => {
                index.insert(key, order.len());
                order.push(r);
            }
        }
    }
    order
}

// =============================================================================
// CodeGraph
// =============================================================================

/// Main CodeGraph class.
///
/// Provides the primary interface for interacting with the code knowledge
/// graph. Like the underlying layers (Rc-backed SQLite handle, RefCell
/// caches), a `CodeGraph` instance is **single-threaded** (`!Send`/`!Sync`) —
/// confine each instance to one thread. The file watcher runs syncs on its
/// own worker thread by opening a fresh, short-lived instance per sync (the
/// cross-process `FileLock` serializes the writes).
pub struct CodeGraph {
    db: RefCell<DatabaseConnection>,
    queries: Rc<QueryBuilder>,
    project_root: PathBuf,
    resolver: ReferenceResolver,
    graph_manager: GraphQueryManager,
    traverser: GraphTraverser,
    context_builder: ContextBuilder,

    /// Mutex for preventing concurrent indexing operations (in-process).
    /// TS used an async `Mutex`; the instance is `!Sync`, so this mostly
    /// powers `is_indexing()` (observable from progress callbacks).
    index_mutex: Mutex<()>,

    /// File lock for preventing concurrent writes across processes
    /// (CLI, MCP, git hooks).
    file_lock: RefCell<FileLock>,

    /// File watcher for auto-sync on file changes.
    watcher: RefCell<Option<FileWatcher>>,

    /// Runtime borrowed from the process boundary. Watcher threads use this
    /// handle to drive async sync work without constructing nested runtimes.
    runtime: Option<tokio::runtime::Handle>,
}

fn segment_lookup_variants(word: &str) -> Vec<String> {
    let mut variants = vec![word.to_string()];
    let char_count = word.chars().count();
    if ["xes", "shes", "sses", "zzes"]
        .iter()
        .any(|ending| word.ends_with(ending))
    {
        if char_count >= 6 {
            variants.push(word[..word.len() - 2].to_string());
        }
    } else if ["ches", "ses", "zes", "oes"]
        .iter()
        .any(|ending| word.ends_with(ending))
    {
        if char_count >= 6 {
            variants.push(word[..word.len() - 2].to_string());
        }
        if char_count >= 5 {
            variants.push(word[..word.len() - 1].to_string());
        }
    } else if word.ends_with('s') && !word.ends_with("ss") && char_count >= 5 {
        variants.push(word[..word.len() - 1].to_string());
    }
    variants
}

fn words_matching_name(name: &str, variant_to_word: &HashMap<String, String>) -> HashSet<String> {
    let segments = split_identifier_segments(name)
        .into_iter()
        .collect::<HashSet<_>>();
    variant_to_word
        .iter()
        .filter(|(variant, _)| segments.contains(*variant))
        .map(|(_, word)| word.clone())
        .collect()
}

impl CodeGraph {
    fn build(
        db: DatabaseConnection,
        queries: Rc<QueryBuilder>,
        project_root: PathBuf,
    ) -> Result<CodeGraph> {
        let resolver = create_resolver(
            project_root.to_string_lossy().to_string(),
            QueryBuilder::new(db.get_db()?),
        );
        let graph_manager = GraphQueryManager::new(Rc::clone(&queries));
        let traverser = GraphTraverser::new(Rc::clone(&queries));
        let context_builder = create_context_builder(
            project_root.clone(),
            Rc::clone(&queries),
            GraphTraverser::new(Rc::clone(&queries)),
        );
        let file_lock = FileLock::new(get_codegraph_dir(&project_root).join("codegraph.lock"));
        Ok(CodeGraph {
            db: RefCell::new(db),
            queries,
            project_root,
            resolver,
            graph_manager,
            traverser,
            context_builder,
            index_mutex: Mutex::new(()),
            file_lock: RefCell::new(file_lock),
            watcher: RefCell::new(None),
            runtime: tokio::runtime::Handle::try_current().ok(),
        })
    }

    // =========================================================================
    // Lifecycle Methods
    // =========================================================================

    /// Initialize a new CodeGraph project.
    ///
    /// Creates the .codegraph directory, database, configuration, and initial
    /// index. Native grammar setup is synchronous; indexing is Tokio-backed.
    pub async fn init(
        project_root: impl AsRef<Path>,
        options: &InitOptions<'_>,
    ) -> Result<CodeGraph> {
        init_grammars();
        let resolved_root = resolve_root(project_root.as_ref());

        // Check if already initialized
        if is_initialized(&resolved_root) {
            return Err(CodeGraphError::other(format!(
                "CodeGraph already initialized in {}",
                resolved_root.display()
            )));
        }

        // Create directory structure
        create_directory(&resolved_root)?;

        // Initialize database
        let db_path = get_database_path(&resolved_root);
        let db = DatabaseConnection::initialize(db_path)?;
        let queries = Rc::new(QueryBuilder::new(db.get_db()?));

        let instance = Self::build(db, queries, resolved_root)?;

        instance
            .index_all(&IndexOptions {
                on_progress: options.on_progress,
                ..Default::default()
            })
            .await?;

        Ok(instance)
    }

    /// Initialize synchronously (without indexing).
    pub fn init_sync(project_root: impl AsRef<Path>) -> Result<CodeGraph> {
        let resolved_root = resolve_root(project_root.as_ref());

        // Check if already initialized
        if is_initialized(&resolved_root) {
            return Err(CodeGraphError::other(format!(
                "CodeGraph already initialized in {}",
                resolved_root.display()
            )));
        }

        // Create directory structure
        create_directory(&resolved_root)?;

        // Initialize database
        let db_path = get_database_path(&resolved_root);
        let db = DatabaseConnection::initialize(db_path)?;
        let queries = Rc::new(QueryBuilder::new(db.get_db()?));

        Self::build(db, queries, resolved_root)
    }

    /// Open an existing CodeGraph project.
    pub async fn open_async(
        project_root: impl AsRef<Path>,
        options: &OpenOptions,
    ) -> Result<CodeGraph> {
        init_grammars();
        let resolved_root = resolve_root(project_root.as_ref());

        // Check if initialized
        if !is_initialized(&resolved_root) {
            return Err(CodeGraphError::other(format!(
                "CodeGraph not initialized in {}. Run init() first.",
                resolved_root.display()
            )));
        }

        // Validate directory structure
        let validation = validate_directory(&resolved_root);
        if !validation.valid {
            return Err(CodeGraphError::other(format!(
                "Invalid CodeGraph directory: {}",
                validation.errors.join(", ")
            )));
        }

        // Open database
        let db_path = get_database_path(&resolved_root);
        let db = DatabaseConnection::open(db_path)?;
        let queries = Rc::new(QueryBuilder::new(db.get_db()?));

        let instance = Self::build(db, queries, resolved_root)?;

        // Sync if requested
        if options.sync {
            instance.sync(&IndexOptions::default()).await?;
        }

        Ok(instance)
    }

    /// Open without running an index sync. Use [`Self::open_async`] when
    /// `OpenOptions::sync` is enabled so the caller's Tokio runtime owns the
    /// asynchronous parsing and resolution work.
    pub fn open(project_root: impl AsRef<Path>, options: &OpenOptions) -> Result<CodeGraph> {
        if options.sync {
            return Err(CodeGraphError::other(
                "OpenOptions::sync requires CodeGraph::open_async",
            ));
        }
        init_grammars();
        Self::open_sync(project_root)
    }

    /// Open synchronously (without sync).
    pub fn open_sync(project_root: impl AsRef<Path>) -> Result<CodeGraph> {
        let resolved_root = resolve_root(project_root.as_ref());

        // Check if initialized
        if !is_initialized(&resolved_root) {
            return Err(CodeGraphError::other(format!(
                "CodeGraph not initialized in {}. Run init() first.",
                resolved_root.display()
            )));
        }

        // Validate directory structure
        let validation = validate_directory(&resolved_root);
        if !validation.valid {
            return Err(CodeGraphError::other(format!(
                "Invalid CodeGraph directory: {}",
                validation.errors.join(", ")
            )));
        }

        // Open database
        let db_path = get_database_path(&resolved_root);
        let db = DatabaseConnection::open(db_path)?;
        let queries = Rc::new(QueryBuilder::new(db.get_db()?));

        Self::build(db, queries, resolved_root)
    }

    /// Check if a directory has been initialized as a CodeGraph project.
    pub fn is_initialized(project_root: impl AsRef<Path>) -> bool {
        is_initialized(&resolve_root(project_root.as_ref()))
    }

    /// Close the CodeGraph instance and release resources.
    pub fn close(&self) {
        self.unwatch();
        // Release file lock if held
        self.file_lock.borrow_mut().release();
        self.db.borrow_mut().close();
    }

    /// Get the project root directory.
    pub fn get_project_root(&self) -> &Path {
        &self.project_root
    }

    // =========================================================================
    // Indexing
    // =========================================================================

    /// Construct the extraction orchestrator over this instance's
    /// QueryBuilder. The TS class kept one orchestrator for its lifetime;
    /// Rust constructs one per operation because `ExtractionOrchestrator<'a>`
    /// borrows the QueryBuilder (self-referential storage otherwise). The
    /// only per-instance state — the lazily-detected framework-name cache —
    /// is therefore re-detected per operation (equal or fresher than TS).
    fn orchestrator(&self) -> ExtractionOrchestrator<'_> {
        ExtractionOrchestrator::new(self.project_root.clone(), self.queries.as_ref())
    }

    async fn lock_index_mutex(&self) -> MutexGuard<'_, ()> {
        self.index_mutex.lock().await
    }

    /// Index all files in the project.
    ///
    /// Uses a mutex to prevent concurrent indexing operations.
    pub async fn index_all(&self, options: &IndexOptions<'_>) -> Result<IndexResult> {
        let _guard = self.lock_index_mutex().await;
        if self.file_lock.borrow_mut().acquire().is_err() {
            return Ok(lock_failure_index_result());
        }
        // Persist this before the first graph read/write. If the process is
        // killed during extraction, resolution, or maintenance, status can
        // distinguish the truncated database from a completed index.
        let _ = self
            .queries
            .set_metadata("index_state", IndexState::Indexing.as_str());
        let mut result = self.index_all_locked(options).await;
        match result.as_mut() {
            Ok(index_result) => self.finalize_full_index_metadata(index_result),
            Err(_) => {
                let _ = self
                    .queries
                    .set_metadata("index_state", IndexState::Failed.as_str());
            }
        }
        self.file_lock.borrow_mut().release();
        result
    }

    fn finalize_full_index_metadata(&self, result: &mut IndexResult) {
        if !result.success {
            let _ = self
                .queries
                .set_metadata("index_state", IndexState::Failed.as_str());
            return;
        }

        // Rebuild from persisted nodes after the scan. Clearing before the scan
        // loses vocabulary when unchanged files are skipped or indexing aborts.
        if let Err(error) = self.queries.rebuild_name_segment_vocab(2_000) {
            result.success = false;
            result.errors.push(ExtractionError {
                message: format!("Failed to rebuild name-segment vocabulary: {error}"),
                file_path: None,
                line: None,
                column: None,
                severity: Severity::Error,
                code: Some("vocabulary_rebuild_failed".to_string()),
            });
            let _ = self
                .queries
                .set_metadata("index_state", IndexState::Failed.as_str());
            return;
        }

        // A targeted sync cannot make an older index current, so these are
        // deliberately written only by this full-index lifecycle.
        if result.files_indexed > 0 {
            let _ = self
                .queries
                .set_metadata("indexed_with_version", env!("CARGO_PKG_VERSION"));
            let _ = self.queries.set_metadata(
                "indexed_with_extraction_version",
                &EXTRACTION_VERSION.to_string(),
            );
        }

        let accounted = result.files_indexed + result.files_skipped + result.files_errored;
        if let Some(discovered) = result.files_discovered {
            let _ = self
                .queries
                .set_metadata("index_files_discovered", &discovered.to_string());
            let _ = self
                .queries
                .set_metadata("index_files_accounted", &accounted.to_string());

            if accounted < discovered {
                let missing = discovered - accounted;
                let _ = self
                    .queries
                    .set_metadata("index_state", IndexState::Partial.as_str());
                result.errors.push(ExtractionError {
                    message: format!(
                        "Index is missing {missing} of {discovered} discovered files (indexed {}, skipped {}, errored {}). The index is PARTIAL - re-run `codegraph index`.",
                        result.files_indexed, result.files_skipped, result.files_errored
                    ),
                    file_path: None,
                    line: None,
                    column: None,
                    severity: Severity::Warning,
                    code: Some("index_partial".to_string()),
                });
                return;
            }
        }

        let _ = self
            .queries
            .set_metadata("index_state", IndexState::Complete.as_str());
    }

    async fn index_all_locked(&self, options: &IndexOptions<'_>) -> Result<IndexResult> {
        let before = self.queries.get_node_and_edge_count()?;
        let orchestrator = self.orchestrator();
        let mut result = orchestrator
            .index_all(options.on_progress, options.signal, options.verbose)
            .await?;
        let reconcile_removed = if result.success {
            orchestrator.reconcile_removed_files()?.files_removed
        } else {
            0
        };
        let touched = result.success && (result.files_indexed > 0 || reconcile_removed > 0);

        // Re-detect frameworks now that the index is populated. The resolver
        // is constructed with createResolver() before any files exist, so
        // framework resolvers whose detect() consults the indexed file list
        // (e.g. UIKit/SwiftUI scanning for imports, swift-objc-bridge looking
        // for both Swift and ObjC files) all return false on that initial pass
        // and silently drop themselves. Re-initializing here gives them a
        // chance to see the actual project before resolution runs.
        if touched {
            self.resolver.initialize();
            // Cross-file finalization (e.g. NestJS RouterModule prefixes). Runs
            // before resolution so updated names show up in subsequent reads.
            self.resolver.run_post_extract();
        }

        // Resolve references to create call/import/extends edges
        if touched {
            // Get count without loading all refs into memory
            let unresolved_count = self.queries.get_unresolved_references_count()? as usize;

            emit_resolving(options.on_progress, 0, unresolved_count);

            let mut cb = |current: usize, total: usize| {
                emit_resolving(options.on_progress, current, total);
            };
            self.resolver
                .resolve_and_persist_batched(Some(&mut cb), None)
                .await?;
        }

        // Refresh planner stats + checkpoint the WAL after bulk writes.
        // Cheap and non-blocking; never load-bearing for correctness.
        if touched {
            self.db.borrow().run_maintenance();
        }

        // The orchestrator only sees extraction-phase counts; resolution and
        // synthesizer edges (often >50% of the graph on JVM repos) come later.
        // Recompute against the DB so the CLI summary reports the true totals.
        if touched {
            let after = self.queries.get_node_and_edge_count()?;
            result.nodes_created = after.nodes.saturating_sub(before.nodes) as usize;
            result.edges_created = after.edges.saturating_sub(before.edges) as usize;
        }

        Ok(result)
    }

    /// Index specific files.
    ///
    /// Uses a mutex to prevent concurrent indexing operations.
    pub async fn index_files(&self, file_paths: &[String]) -> Result<IndexResult> {
        let _guard = self.lock_index_mutex().await;
        if self.file_lock.borrow_mut().acquire().is_err() {
            return Ok(lock_failure_index_result());
        }
        let result = self.index_files_locked(file_paths).await;
        self.file_lock.borrow_mut().release();
        result
    }

    async fn index_files_locked(&self, file_paths: &[String]) -> Result<IndexResult> {
        let before = self.queries.get_node_and_edge_count()?;

        // Symbol names present BEFORE re-indexing. A symbol that the edit
        // removed or renamed only exists in this pre-state — collecting names
        // after the re-index alone would skip it, leaving stale references to
        // the old name unrepaired.
        let mut changed_node_names = Vec::new();
        for file_path in file_paths {
            for node in self.queries.get_nodes_by_file(file_path)? {
                if !changed_node_names.contains(&node.name) {
                    changed_node_names.push(node.name);
                }
            }
        }

        let orchestrator = self.orchestrator();
        let mut result = orchestrator.index_files(file_paths).await?;
        let touched = result.success && result.files_indexed > 0;

        if touched {
            orchestrator.reset_detected_frameworks();
            self.resolver.initialize();
            self.resolver.run_post_extract();

            // Union in the post-index names (newly added or renamed-to
            // symbols) so both sides of a rename get their references
            // re-resolved.
            for file_path in file_paths {
                for node in self.queries.get_nodes_by_file(file_path)? {
                    if !changed_node_names.contains(&node.name) {
                        changed_node_names.push(node.name);
                    }
                }
            }

            let by_file = self
                .queries
                .get_unresolved_references_by_files(file_paths)?;
            let by_name = self
                .queries
                .get_unresolved_references_by_names(&changed_node_names)?;
            let unresolved_refs = dedupe_unresolved_refs(by_file, by_name);
            self.resolver
                .resolve_and_persist(&unresolved_refs, None)
                .await?;

            self.db.borrow().run_maintenance();

            let after = self.queries.get_node_and_edge_count()?;
            result.nodes_created = after.nodes.saturating_sub(before.nodes) as usize;
            result.edges_created = after.edges.saturating_sub(before.edges) as usize;
        }

        Ok(result)
    }

    /// Sync with current file state (incremental update).
    ///
    /// Uses a mutex to prevent concurrent indexing operations.
    pub async fn sync(&self, options: &IndexOptions<'_>) -> Result<SyncResult> {
        let _guard = self.lock_index_mutex().await;
        if self.file_lock.borrow_mut().acquire().is_err() {
            return Ok(lock_failure_sync_result());
        }
        let result = self.sync_locked(options).await;
        self.file_lock.borrow_mut().release();
        result
    }

    async fn sync_locked(&self, options: &IndexOptions<'_>) -> Result<SyncResult> {
        let vocab_was_empty = self.queries.is_name_segment_vocab_empty().unwrap_or(false);
        let orchestrator = self.orchestrator();
        let result = orchestrator.sync(options.on_progress).await?;

        let touched =
            result.files_added > 0 || result.files_modified > 0 || result.files_removed > 0;

        if touched {
            orchestrator.reset_detected_frameworks();
            self.resolver.initialize();
        }

        // Cross-file finalization (e.g. NestJS RouterModule prefixes). Run on
        // every sync that touched files so edits to `app.module.ts` propagate
        // to controllers in unchanged files. The pass is idempotent and cheap
        // (regex over *.module.ts only).
        if touched {
            self.resolver.run_post_extract();
        }

        // Resolve references if files were updated
        if result.files_added > 0 || result.files_modified > 0 {
            if result.changed_file_paths.is_some() || result.changed_node_names.is_some() {
                let by_file = match &result.changed_file_paths {
                    Some(paths) => self.queries.get_unresolved_references_by_files(paths)?,
                    None => Vec::new(),
                };
                let by_name = match &result.changed_node_names {
                    Some(names) => self.queries.get_unresolved_references_by_names(names)?,
                    None => Vec::new(),
                };
                let unresolved_refs = dedupe_unresolved_refs(by_file, by_name);

                emit_resolving(options.on_progress, 0, unresolved_refs.len());

                let mut cb = |current: usize, total: usize| {
                    emit_resolving(options.on_progress, current, total);
                };
                self.resolver
                    .resolve_and_persist(&unresolved_refs, Some(&mut cb))
                    .await?;
            } else {
                // No git info — use batched resolution to avoid OOM
                let unresolved_count = self.queries.get_unresolved_references_count()? as usize;

                emit_resolving(options.on_progress, 0, unresolved_count);

                let mut cb = |current: usize, total: usize| {
                    emit_resolving(options.on_progress, current, total);
                };
                self.resolver
                    .resolve_and_persist_batched(Some(&mut cb), None)
                    .await?;
            }
        }

        // Migrated databases start with an empty vocabulary. Incremental node
        // writes only cover changed files, so heal the unchanged bulk here.
        if vocab_was_empty && self.queries.get_node_and_edge_count()?.nodes > 0 {
            self.queries.rebuild_name_segment_vocab(2_000)?;
        }

        // Refresh planner stats + checkpoint the WAL after bulk writes.
        if touched {
            self.db.borrow().run_maintenance();
        }

        Ok(result)
    }

    /// Check if an indexing operation is currently in progress.
    pub fn is_indexing(&self) -> bool {
        self.index_mutex.try_lock().is_err()
    }

    /// One-shot upgrade heal for read-mostly callers such as the prompt hook.
    /// Returns false only for an empty graph or when another writer owns the
    /// project lock and will perform the same heal itself.
    pub fn heal_segment_vocab_if_empty(&self) -> Result<bool> {
        if !self.queries.is_name_segment_vocab_empty()? {
            return Ok(true);
        }
        if self.queries.get_node_and_edge_count()?.nodes == 0 {
            return Ok(false);
        }

        let Ok(_guard) = self.index_mutex.try_lock() else {
            return Ok(false);
        };
        if self.file_lock.borrow_mut().acquire().is_err() {
            return Ok(false);
        }
        let result = self
            .queries
            .is_name_segment_vocab_empty()
            .and_then(|empty| {
                if empty {
                    self.queries.rebuild_name_segment_vocab(2_000)
                } else {
                    Ok(())
                }
            });
        self.file_lock.borrow_mut().release();
        result.map(|()| true)
    }

    /// Match prompt prose to live symbols through the materialized identifier
    /// segment vocabulary. Two words on one name are strong evidence; a lone
    /// word must be both rare and repeated across names in this repository.
    pub fn get_segment_matches(&self, words: &[String], limit: usize) -> Result<Vec<SegmentMatch>> {
        const SEGMENT_RARITY_CEILING: usize = 25;
        if words.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }

        let mut variant_to_word = HashMap::new();
        let mut variants = Vec::new();
        for word in words {
            for variant in segment_lookup_variants(word) {
                if !variant_to_word.contains_key(&variant) {
                    variant_to_word.insert(variant.clone(), word.clone());
                    variants.push(variant);
                }
            }
        }
        let variant_pairs = variants
            .iter()
            .map(|variant| (variant.clone(), variant_to_word[variant].clone()))
            .collect::<Vec<_>>();
        let mut candidates: Vec<(String, HashSet<String>)> = Vec::new();
        for (name, _) in self
            .queries
            .get_segment_co_occurrence(&variant_pairs, 2, 24)?
        {
            let matched = words_matching_name(&name, &variant_to_word);
            if matched.len() >= 2 {
                candidates.push((name, matched));
            }
        }

        if candidates.is_empty() {
            let single_variants = variants
                .iter()
                .filter(|variant| variant_to_word[*variant].chars().count() >= 5)
                .cloned()
                .collect::<Vec<_>>();
            let counts = self.queries.get_segment_name_counts(&single_variants)?;
            let mut rare = counts
                .into_iter()
                .filter(|(_, count)| *count >= 2 && *count <= SEGMENT_RARITY_CEILING)
                .collect::<Vec<_>>();
            rare.sort_by(|(left_name, left_count), (right_name, right_count)| {
                left_count
                    .cmp(right_count)
                    .then_with(|| left_name.cmp(right_name))
            });
            for (variant, _) in rare.into_iter().take(2) {
                let original = variant_to_word[&variant].clone();
                for name in self.queries.get_names_for_segment(&variant, 12)? {
                    if split_identifier_segments(&name).len() >= 2 {
                        candidates.push((name, HashSet::from([original.clone()])));
                    }
                }
            }
        }

        candidates.sort_by(|(left_name, left_words), (right_name, right_words)| {
            right_words
                .len()
                .cmp(&left_words.len())
                .then_with(|| left_name.len().cmp(&right_name.len()))
                .then_with(|| left_name.cmp(right_name))
        });
        let mut seen = HashSet::new();
        let mut output = Vec::new();
        for (name, matched_words) in candidates {
            if output.len() >= limit || !seen.insert(name.clone()) {
                continue;
            }
            let Some(node) = self
                .queries
                .get_nodes_by_name(&name)?
                .into_iter()
                .find(|node| !matches!(node.kind, NodeKind::File | NodeKind::Import))
            else {
                continue;
            };
            let mut matched_words = matched_words.into_iter().collect::<Vec<_>>();
            matched_words.sort();
            output.push(SegmentMatch {
                name,
                kind: node.kind,
                file_path: node.file_path,
                start_line: node.start_line,
                matched_words,
            });
        }
        Ok(output)
    }

    // =========================================================================
    // File Watching
    // =========================================================================

    /// Start watching for file changes and auto-syncing.
    ///
    /// Uses native OS file events (FSEvents on macOS, inotify on Linux,
    /// ReadDirectoryChangesW on Windows) with debouncing to avoid thrashing.
    ///
    /// Returns `true` if watching started successfully.
    ///
    /// Port note: the debounced sync runs on the watcher's worker thread,
    /// which opens a fresh short-lived `CodeGraph` per sync (this instance is
    /// `!Send`). The cross-process `FileLock` serializes writes — when this
    /// instance (or another process) holds it, the worker's sync returns the
    /// lock-failure zero-shape, surfaced as `LockUnavailableError` so the
    /// watcher keeps pendingFiles + reschedules instead of clearing them (#449).
    pub fn watch(&self, options: WatchOptions) -> bool {
        if let Some(watcher) = self.watcher.borrow().as_ref() {
            if watcher.is_active() {
                return true;
            }
        }

        let Some(runtime) = self.runtime.clone() else {
            return false;
        };
        let root = self.project_root.clone();
        let sync_fn: SyncFn = Arc::new(move || {
            let cg = CodeGraph::open_sync(&root).map_err(|e| Box::new(e) as SyncError)?;
            let result = runtime.block_on(cg.sync(&IndexOptions::default()));
            cg.close();
            let result = result.map_err(|e| Box::new(e) as SyncError)?;
            // sync() returns this exact zero-shape iff it failed to acquire the
            // file lock (a real empty sync always has filesChecked > 0 because
            // scanDirectory ran). Surface that to the watcher as a typed error
            // so it keeps pendingFiles + reschedules instead of clearing them
            // (#449).
            if result.files_checked == 0 && result.duration_ms == 0 {
                return Err(Box::new(LockUnavailableError::new()) as SyncError);
            }
            let files_changed = result.files_added + result.files_modified + result.files_removed;
            Ok(WatchSyncResult {
                files_changed,
                duration_ms: result.duration_ms,
            })
        });

        let watcher = FileWatcher::new(self.project_root.clone(), sync_fn, options);
        let started = watcher.start();
        *self.watcher.borrow_mut() = Some(watcher);
        started
    }

    /// Stop watching for file changes.
    pub fn unwatch(&self) {
        if let Some(watcher) = self.watcher.borrow_mut().take() {
            watcher.stop();
        }
    }

    /// Check if the file watcher is active.
    pub fn is_watching(&self) -> bool {
        self.watcher
            .borrow()
            .as_ref()
            .map(|w| w.is_active())
            .unwrap_or(false)
    }

    /// Files seen by the file watcher since the last successful sync —
    /// the per-file "stale" signal MCP tools attach to responses so an agent
    /// can fall back to Read for just the affected file without waiting for a
    /// debounced sync to complete (issue #403).
    ///
    /// Returns an empty list when the watcher isn't active, or no events have
    /// arrived. Each entry includes `firstSeenMs` and `lastSeenMs` (wall-clock
    /// `Date.now()` values) so callers can render "edited Nms ago", plus an
    /// `indexing` flag indicating whether the in-flight sync (if any) will
    /// absorb that file.
    pub fn get_pending_files(&self) -> Vec<PendingFile> {
        self.watcher
            .borrow()
            .as_ref()
            .map(|w| w.get_pending_files())
            .unwrap_or_default()
    }

    /// Resolves once the file watcher has installed its watch set. Useful for
    /// tests that need a deterministic boundary before asserting on
    /// `get_pending_files()`. Returns immediately when no watcher is active.
    ///
    /// `timeout_ms: None` uses the watcher default (10 000 ms).
    pub fn wait_until_watcher_ready(&self, timeout_ms: Option<u64>) -> Result<()> {
        match self.watcher.borrow().as_ref() {
            Some(w) => w.wait_until_ready(timeout_ms.unwrap_or(DEFAULT_READY_TIMEOUT_MS)),
            None => Ok(()),
        }
    }

    /// Get files that have changed since last index.
    pub fn get_changed_files(&self) -> Result<ChangedFiles> {
        self.orchestrator().get_changed_files()
    }

    /// Most recent index timestamp (ms since epoch) across all tracked files,
    /// or `None` when nothing is indexed yet. Lets library consumers check
    /// index freshness without shelling out to `codegraph status --json`. (#329)
    pub fn get_last_indexed_at(&self) -> Result<Option<i64>> {
        self.queries.get_last_indexed_at()
    }

    /// Completeness marker left by the most recent full-index attempt.
    pub fn get_index_state(&self) -> Result<Option<IndexState>> {
        Ok(self
            .queries
            .get_metadata("index_state")?
            .as_deref()
            .and_then(IndexState::parse))
    }

    /// Engine and extraction versions stamped by the most recent non-empty
    /// successful full index.
    pub fn get_index_build_info(&self) -> Result<IndexBuildInfo> {
        let version = self.queries.get_metadata("indexed_with_version")?;
        let extraction_version = self
            .queries
            .get_metadata("indexed_with_extraction_version")?
            .and_then(|value| value.parse::<u32>().ok());
        Ok(IndexBuildInfo {
            version,
            extraction_version,
        })
    }

    /// Whether an existing graph predates the current extraction semantics.
    pub fn is_index_stale(&self) -> Result<bool> {
        if self.get_last_indexed_at()?.is_none() {
            return Ok(false);
        }
        Ok(self
            .get_index_build_info()?
            .extraction_version
            .is_none_or(|version| version < EXTRACTION_VERSION))
    }

    /// Extract nodes and edges from source code (without storing).
    pub fn extract_from_source(&self, file_path: &str, source: &str) -> ExtractionResult {
        let config = crate::project_config::load_project_config(&self.project_root);
        let language = crate::extraction::detect_language_with_overrides(
            file_path,
            Some(source),
            config.extension_overrides(),
        );
        extract_from_source(file_path, source, Some(language), None)
    }

    // =========================================================================
    // Reference Resolution
    // =========================================================================

    /// Resolve unresolved references and create edges.
    ///
    /// This method takes unresolved references from extraction and attempts
    /// to resolve them using multiple strategies:
    /// - Framework-specific patterns (React, Express, Laravel)
    /// - Import-based resolution
    /// - Name-based symbol matching
    pub async fn resolve_references(
        &self,
        on_progress: Option<&mut dyn FnMut(usize, usize)>,
    ) -> Result<ResolutionResult> {
        // Get all unresolved references from the database
        let unresolved_refs = self.queries.get_unresolved_references()?;
        self.resolver
            .resolve_and_persist(&unresolved_refs, on_progress)
            .await
    }

    /// Resolve references in batches to keep memory bounded on large
    /// codebases. Processes chunks of unresolved refs, persisting results
    /// after each batch.
    pub async fn resolve_references_batched(
        &self,
        on_progress: Option<&mut dyn FnMut(usize, usize)>,
    ) -> Result<ResolutionResult> {
        self.resolver
            .resolve_and_persist_batched(on_progress, None)
            .await
    }

    /// Get detected frameworks in the project.
    pub fn get_detected_frameworks(&self) -> Vec<String> {
        self.resolver.get_detected_frameworks()
    }

    /// Re-initialize the resolver (useful after adding new files).
    pub fn reinitialize_resolver(&self) {
        self.resolver.initialize();
    }

    // =========================================================================
    // Graph Statistics
    // =========================================================================

    /// Get statistics about the knowledge graph.
    pub fn get_stats(&self) -> Result<GraphStats> {
        let mut stats = self.queries.get_stats()?;
        stats.db_size_bytes = self.db.borrow().get_size()?;
        Ok(stats)
    }

    /// Active SQLite backend for this project's connection (`"native"`).
    /// Surfaced via `codegraph status` and the `codegraph_status` MCP tool
    /// alongside the effective journal mode.
    pub fn get_backend(&self) -> SqliteBackend {
        self.db.borrow().get_backend()
    }

    /// The journal mode actually in effect ('wal', 'delete', …). 'wal' means
    /// readers never block on a concurrent writer; anything else means they
    /// can, which is the precondition for the "database is locked" failures in
    /// issue #238. Surfaced via `codegraph status` and the `codegraph_status`
    /// MCP tool.
    pub fn get_journal_mode(&self) -> Result<String> {
        self.db.borrow().get_journal_mode()
    }

    // =========================================================================
    // Resolution benchmarking
    // =========================================================================

    /// Run the resolution pass over up to `limit` pending unresolved
    /// references WITHOUT persisting edges or deleting refs, and return a
    /// throughput report. Hidden harness behind `codegraph resolve-bench` so
    /// resolver changes (memoization, parallelism, GPU offload) are measured
    /// against real project databases instead of microbenchmarks.
    pub async fn resolve_bench(&self, limit: usize) -> Result<String> {
        let _guard = self.lock_index_mutex().await;

        // Page refs in stable id order, exactly like the real resolve pass.
        let mut refs = Vec::new();
        let mut last_id = 0i64;
        while refs.len() < limit {
            let page_size = (limit - refs.len()).min(5000);
            let page = self
                .queries
                .get_unresolved_references_batch_after_id(last_id, page_size)?;
            if page.refs.is_empty() {
                break;
            }
            last_id = page.last_id;
            refs.extend(page.refs);
        }
        if refs.is_empty() {
            return Ok("no pending unresolved references in this index".to_string());
        }

        // Same setup the real pass performs (framework detection + caches).
        self.resolver.initialize();
        let load_start = std::time::Instant::now();
        self.resolver.warm_caches();
        let warm_ms = load_start.elapsed().as_millis();

        let start = std::time::Instant::now();
        let result = self.resolver.resolve_all_parallel(&refs, None).await?;
        let elapsed = start.elapsed();

        let per_ref_us = elapsed.as_micros() as f64 / refs.len() as f64;
        let mut by_method: Vec<(String, usize)> = result.stats.by_method.into_iter().collect();
        by_method.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        Ok(format!(
            "refs: {} | warm: {} ms | resolve: {:.2} s | {:.1} µs/ref | {:.0} refs/s\nresolved: {} | unresolved: {}\nby_method: {:?}",
            refs.len(),
            warm_ms,
            elapsed.as_secs_f64(),
            per_ref_us,
            refs.len() as f64 / elapsed.as_secs_f64(),
            result.stats.resolved,
            result.stats.unresolved,
            by_method
        ))
    }

    // =========================================================================
    // Node Operations
    // =========================================================================

    /// Get a node by ID.
    pub fn get_node(&self, id: &str) -> Result<Option<Node>> {
        self.queries.get_node_by_id(id)
    }

    /// Get all nodes in a file.
    pub fn get_nodes_in_file(&self, file_path: &str) -> Result<Vec<Node>> {
        self.queries.get_nodes_by_file(file_path)
    }

    /// Get all nodes of a specific kind.
    pub fn get_nodes_by_kind(&self, kind: NodeKind) -> Result<Vec<Node>> {
        self.queries.get_nodes_by_kind(kind)
    }

    /// Get ALL nodes with an exact name (direct index lookup, not
    /// FTS-ranked/capped). Used to enumerate every overload of a
    /// heavily-overloaded name so the specific definition the caller wants is
    /// never dropped below a search cut.
    pub fn get_nodes_by_name(&self, name: &str) -> Result<Vec<Node>> {
        self.queries.get_nodes_by_name(name)
    }

    /// Search nodes by text.
    pub fn search_nodes(
        &self,
        query: &str,
        options: Option<&SearchOptions>,
    ) -> Result<Vec<SearchResult>> {
        let default_options = SearchOptions::default();
        self.queries
            .search_nodes(query, options.unwrap_or(&default_options))
    }

    /// Find the project's "primary route file" — the file with the densest
    /// concentration of framework-emitted `route` nodes (≥3 routes, ≥30%
    /// of all non-test routes). Used to inline the routing config in
    /// `codegraph_explore` responses on small realworld template repos.
    pub fn get_top_route_file(&self) -> Result<Option<TopRouteFile>> {
        self.queries.get_top_route_file()
    }

    /// Build a URL → handler routing manifest from the index. Each entry
    /// pairs a route node (URL + method) with its handler function/method
    /// via the `references` edge that framework resolvers emit. Returns
    /// `None` when fewer than 3 valid (non-test) routes exist.
    pub fn get_routing_manifest(&self, limit: Option<usize>) -> Result<Option<RoutingManifest>> {
        self.queries.get_routing_manifest(limit)
    }

    // =========================================================================
    // Edge Operations
    // =========================================================================

    /// Get outgoing edges from a node.
    pub fn get_outgoing_edges(&self, node_id: &str) -> Result<Vec<Edge>> {
        self.queries.get_outgoing_edges(node_id, None, None)
    }

    /// Get incoming edges to a node.
    pub fn get_incoming_edges(&self, node_id: &str) -> Result<Vec<Edge>> {
        self.queries.get_incoming_edges(node_id, None)
    }

    // =========================================================================
    // File Operations
    // =========================================================================

    /// Get a file record by path.
    pub fn get_file(&self, file_path: &str) -> Result<Option<FileRecord>> {
        self.queries.get_file_by_path(file_path)
    }

    /// Get all tracked files.
    pub fn get_files(&self) -> Result<Vec<FileRecord>> {
        self.queries.get_all_files()
    }

    // =========================================================================
    // Graph Query Methods
    // =========================================================================

    /// Get the context for a node (ancestors, children, references).
    ///
    /// Returns comprehensive context about a node including its containment
    /// hierarchy, children, incoming/outgoing references, type information,
    /// and relevant imports.
    pub fn get_context(&self, node_id: &str) -> Result<Context> {
        self.graph_manager.get_context(node_id)
    }

    /// Traverse the graph from a starting node.
    ///
    /// Uses breadth-first search. Supports filtering by edge types, node
    /// types, and traversal direction. `None` options use the defaults.
    pub fn traverse(&self, start_id: &str, options: Option<&TraversalOptions>) -> Result<Subgraph> {
        let default_options = TraversalOptions::default();
        self.traverser
            .traverse_bfs(start_id, options.unwrap_or(&default_options))
    }

    /// Get the call graph for a function.
    ///
    /// Returns both callers (functions that call this function) and
    /// callees (functions called by this function) up to the specified depth.
    /// `depth: None` = TS default 2.
    pub fn get_call_graph(&self, node_id: &str, depth: Option<u32>) -> Result<Subgraph> {
        self.traverser.get_call_graph(node_id, depth.unwrap_or(2))
    }

    /// Get the type hierarchy for a class/interface.
    ///
    /// Returns both ancestors (types this extends/implements) and
    /// descendants (types that extend/implement this).
    pub fn get_type_hierarchy(&self, node_id: &str) -> Result<Subgraph> {
        self.traverser.get_type_hierarchy(node_id)
    }

    /// Find all usages of a symbol.
    ///
    /// Returns all nodes that reference the specified symbol through
    /// any edge type (calls, references, type_of, etc.).
    pub fn find_usages(&self, node_id: &str) -> Result<Vec<NodeRef>> {
        self.traverser.find_usages(node_id)
    }

    /// Get callers of a function/method. `max_depth: None` = TS default 1.
    pub fn get_callers(&self, node_id: &str, max_depth: Option<u32>) -> Result<Vec<NodeRef>> {
        self.traverser.get_callers(node_id, max_depth.unwrap_or(1))
    }

    /// Get callees of a function/method. `max_depth: None` = TS default 1.
    pub fn get_callees(&self, node_id: &str, max_depth: Option<u32>) -> Result<Vec<NodeRef>> {
        self.traverser.get_callees(node_id, max_depth.unwrap_or(1))
    }

    /// Calculate the impact radius of a node.
    ///
    /// Returns all nodes that could be affected by changes to this node.
    /// `max_depth: None` = TS default 3.
    pub fn get_impact_radius(&self, node_id: &str, max_depth: Option<u32>) -> Result<Subgraph> {
        self.traverser
            .get_impact_radius(node_id, max_depth.unwrap_or(3))
    }

    /// Find the shortest path between two nodes.
    ///
    /// `edge_kinds: None` (TS default `[]`) considers all edge types.
    /// Returns `None` if no path exists.
    pub fn find_path(
        &self,
        from_id: &str,
        to_id: &str,
        edge_kinds: Option<&[EdgeKind]>,
    ) -> Result<Option<Vec<PathStep>>> {
        self.traverser
            .find_path(from_id, to_id, edge_kinds.unwrap_or(&[]))
    }

    /// Get ancestors of a node in the containment hierarchy
    /// (immediate parent to root).
    pub fn get_ancestors(&self, node_id: &str) -> Result<Vec<Node>> {
        self.traverser.get_ancestors(node_id)
    }

    /// Get immediate children of a node.
    pub fn get_children(&self, node_id: &str) -> Result<Vec<Node>> {
        self.traverser.get_children(node_id)
    }

    /// Get dependencies of a file (file paths this file depends on).
    pub fn get_file_dependencies(&self, file_path: &str) -> Result<Vec<String>> {
        self.graph_manager.get_file_dependencies(file_path)
    }

    /// Get dependents of a file (file paths that depend on this file).
    pub fn get_file_dependents(&self, file_path: &str) -> Result<Vec<String>> {
        self.graph_manager.get_file_dependents(file_path)
    }

    /// Find circular dependencies in the codebase.
    /// Each cycle is an array of file paths.
    pub fn find_circular_dependencies(&self) -> Result<Vec<Vec<String>>> {
        self.graph_manager.find_circular_dependencies()
    }

    /// Find dead code (unreferenced symbols).
    ///
    /// `kinds: None` = TS default (functions, methods, classes).
    pub fn find_dead_code(&self, kinds: Option<&[NodeKind]>) -> Result<Vec<Node>> {
        self.graph_manager.find_dead_code(kinds)
    }

    /// Get complexity metrics for a node.
    pub fn get_node_metrics(&self, node_id: &str) -> Result<NodeMetrics> {
        self.graph_manager.get_node_metrics(node_id)
    }

    // =========================================================================
    // Context Building
    // =========================================================================

    /// Get the source code for a node.
    ///
    /// Reads the file and extracts the code between startLine and endLine.
    /// Returns `None` if the node (or its file) is not found.
    pub fn get_code(&self, node_id: &str) -> Result<Option<String>> {
        self.context_builder.get_code(node_id)
    }

    /// Find relevant subgraph for a query.
    ///
    /// Combines semantic search with graph traversal to find the most
    /// relevant nodes and their relationships for a given query.
    pub fn find_relevant_context(
        &self,
        query: &str,
        options: Option<&FindRelevantContextOptions>,
    ) -> Result<Subgraph> {
        let default_options = FindRelevantContextOptions::default();
        self.context_builder
            .find_relevant_context(query, options.unwrap_or(&default_options))
    }

    /// Build context for a task.
    ///
    /// Creates comprehensive context by:
    /// 1. Running FTS search to find entry points
    /// 2. Expanding the graph around entry points
    /// 3. Extracting code blocks for key nodes
    /// 4. Formatting output for Claude
    ///
    /// Returns the formatted string (markdown by default, or JSON via
    /// `options.format`). Use [`CodeGraph::build_task_context`] for the
    /// structured `TaskContext` (the TS `TaskContext | string` raw path).
    pub fn build_context(
        &self,
        input: &TaskInput,
        options: Option<&BuildContextOptions>,
    ) -> Result<String> {
        let default_options = BuildContextOptions::default();
        self.context_builder
            .build_context(input, options.unwrap_or(&default_options))
    }

    /// Structured variant of [`CodeGraph::build_context`] — returns the
    /// `TaskContext` object instead of a formatted string.
    pub fn build_task_context(
        &self,
        input: &TaskInput,
        options: Option<&BuildContextOptions>,
    ) -> Result<TaskContext> {
        let default_options = BuildContextOptions::default();
        self.context_builder
            .build_task_context(input, options.unwrap_or(&default_options))
    }

    // =========================================================================
    // Database Management
    // =========================================================================

    /// Optimize the database (vacuum and analyze).
    pub fn optimize(&self) -> Result<()> {
        self.db.borrow().optimize()
    }

    /// Clear all data from the graph.
    pub fn clear(&self) -> Result<()> {
        self.queries.clear()
    }

    /// Alias for close() for backwards compatibility.
    #[deprecated(note = "Use close() instead")]
    pub fn destroy(&self) {
        self.close();
    }

    /// Completely remove CodeGraph from the project.
    /// This closes the database and deletes the .codegraph directory.
    ///
    /// WARNING: This permanently deletes all CodeGraph data for the project.
    pub fn uninitialize(&self) -> Result<()> {
        self.close();
        remove_directory(&self.project_root)
    }
}

impl std::fmt::Debug for CodeGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeGraph")
            .field("project_root", &self.project_root)
            .finish_non_exhaustive()
    }
}

impl Drop for CodeGraph {
    fn drop(&mut self) {
        // TS relied on explicit close(); Drop makes the watcher worker stop
        // and the file lock release deterministic for Rust consumers that
        // forget. close() is idempotent.
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedupe_keeps_first_position_and_last_value() {
        let make = |from: &str, name: &str, line: u32| UnresolvedReference {
            from_node_id: from.to_string(),
            reference_name: name.to_string(),
            reference_kind: EdgeKind::Calls,
            line,
            column: 0,
            file_path: None,
            language: None,
            candidates: None,
            metadata: None,
        };
        let by_file = vec![make("a", "x", 1), make("b", "y", 2)];
        let by_name = vec![make("a", "x", 9), make("c", "z", 3)];
        let merged = dedupe_unresolved_refs(by_file, by_name);
        assert_eq!(merged.len(), 3);
        // First position kept (index 0), value replaced by the later duplicate
        assert_eq!(merged[0].from_node_id, "a");
        assert_eq!(merged[0].line, 9);
        assert_eq!(merged[1].from_node_id, "b");
        assert_eq!(merged[2].from_node_id, "c");
    }

    #[test]
    fn lock_failure_shapes_match_ts() {
        let index = lock_failure_index_result();
        assert!(!index.success);
        assert_eq!(
            index.errors[0].message,
            "Could not acquire file lock - another process may be indexing"
        );
        assert_eq!(index.errors[0].severity, Severity::Error);

        let sync = lock_failure_sync_result();
        assert_eq!(sync.files_checked, 0);
        assert_eq!(sync.duration_ms, 0);
        assert!(sync.changed_file_paths.is_none());
    }
}

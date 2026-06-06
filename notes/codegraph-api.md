# public-API module port notes (CodeGraph facade)

Ported `src/index.ts` (1079 ln) → `rust/src/codegraph.rs`. Tests:
`rust/tests/codegraph_api_test.rs` (42, all passing) + 2 in-module unit tests.
Verification at handoff: `cargo check --all-targets` clean (zero warnings
crate-wide), full `cargo test` green (375 lib tests + every integration suite).

Files touched: ONLY `src/codegraph.rs`, `tests/codegraph_api_test.rs`, and this
notes file. No foreign module files modified; `lib.rs`/`Cargo.toml` untouched.

## Public API surface

```rust
// Options (TS interfaces; TS optional params → Option fields, derive Default)
pub struct InitOptions<'a> { pub index: bool,
    pub on_progress: Option<&'a dyn Fn(&IndexProgress)> }
pub struct OpenOptions { pub sync: bool, pub read_only: bool } // read_only declared-but-unused, TS parity
pub struct IndexOptions<'a> {
    pub on_progress: Option<&'a dyn Fn(&IndexProgress)>,
    pub signal: Option<&'a AtomicBool>,   // TS AbortSignal; true = aborted
    pub verbose: bool }

pub struct CodeGraph;  // !Send/!Sync (Rc-backed Db + RefCell) — one thread per instance
impl CodeGraph {
    // Lifecycle (TS async init/open are sync here; grammars are native)
    pub fn init(root: impl AsRef<Path>, options: &InitOptions) -> Result<CodeGraph>;
    pub fn init_sync(root: impl AsRef<Path>) -> Result<CodeGraph>;
    pub fn open(root: impl AsRef<Path>, options: &OpenOptions) -> Result<CodeGraph>;
    pub fn open_sync(root: impl AsRef<Path>) -> Result<CodeGraph>;
    pub fn is_initialized(root: impl AsRef<Path>) -> bool;
    pub fn close(&self);                          // idempotent; also runs on Drop
    pub fn get_project_root(&self) -> &Path;

    // Indexing
    pub fn index_all(&self, options: &IndexOptions) -> Result<IndexResult>;
    pub fn index_files(&self, file_paths: &[String]) -> Result<IndexResult>;
    pub fn sync(&self, options: &IndexOptions) -> Result<SyncResult>;
    pub fn is_indexing(&self) -> bool;

    // Watching
    pub fn watch(&self, options: WatchOptions) -> bool;   // sync::WatchOptions
    pub fn unwatch(&self);
    pub fn is_watching(&self) -> bool;
    pub fn get_pending_files(&self) -> Vec<PendingFile>;
    pub fn wait_until_watcher_ready(&self, timeout_ms: Option<u64>) -> Result<()>; // None = 10_000
    pub fn get_changed_files(&self) -> Result<ChangedFiles>;
    pub fn get_last_indexed_at(&self) -> Result<Option<i64>>;        // #329
    pub fn extract_from_source(&self, file_path: &str, source: &str) -> ExtractionResult;

    // Resolution
    pub fn resolve_references(&self, on_progress: Option<&mut dyn FnMut(usize, usize)>)
        -> Result<ResolutionResult>;
    pub fn resolve_references_batched(&self, on_progress: Option<&mut dyn FnMut(usize, usize)>)
        -> Result<ResolutionResult>;
    pub fn get_detected_frameworks(&self) -> Vec<String>;
    pub fn reinitialize_resolver(&self);

    // Stats / DB
    pub fn get_stats(&self) -> Result<GraphStats>;        // fills db_size_bytes
    pub fn get_backend(&self) -> SqliteBackend;           // "native"
    pub fn get_journal_mode(&self) -> Result<String>;     // "wal"
    pub fn optimize(&self) -> Result<()>;
    pub fn clear(&self) -> Result<()>;
    #[deprecated] pub fn destroy(&self);                  // alias for close()
    pub fn uninitialize(&self) -> Result<()>;             // close + remove .codegraph/

    // Nodes / edges / files (thin QueryBuilder passthroughs)
    pub fn get_node(&self, id: &str) -> Result<Option<Node>>;
    pub fn get_nodes_in_file(&self, file_path: &str) -> Result<Vec<Node>>;
    pub fn get_nodes_by_kind(&self, kind: NodeKind) -> Result<Vec<Node>>;
    pub fn get_nodes_by_name(&self, name: &str) -> Result<Vec<Node>>;
    pub fn search_nodes(&self, query: &str, options: Option<&SearchOptions>) -> Result<Vec<SearchResult>>;
    pub fn get_top_route_file(&self) -> Result<Option<TopRouteFile>>;
    pub fn get_routing_manifest(&self, limit: Option<usize>) -> Result<Option<RoutingManifest>>;
    pub fn get_outgoing_edges(&self, node_id: &str) -> Result<Vec<Edge>>;
    pub fn get_incoming_edges(&self, node_id: &str) -> Result<Vec<Edge>>;
    pub fn get_file(&self, file_path: &str) -> Result<Option<FileRecord>>;
    pub fn get_files(&self) -> Result<Vec<FileRecord>>;

    // Graph queries (TS default args → Option, None = TS default, listed)
    pub fn get_context(&self, node_id: &str) -> Result<Context>; // Err "Node not found: <id>"
    pub fn traverse(&self, start_id: &str, options: Option<&TraversalOptions>) -> Result<Subgraph>;
    pub fn get_call_graph(&self, node_id: &str, depth: Option<u32>) -> Result<Subgraph>;        // 2
    pub fn get_type_hierarchy(&self, node_id: &str) -> Result<Subgraph>;
    pub fn find_usages(&self, node_id: &str) -> Result<Vec<NodeRef>>;
    pub fn get_callers(&self, node_id: &str, max_depth: Option<u32>) -> Result<Vec<NodeRef>>;   // 1
    pub fn get_callees(&self, node_id: &str, max_depth: Option<u32>) -> Result<Vec<NodeRef>>;   // 1
    pub fn get_impact_radius(&self, node_id: &str, max_depth: Option<u32>) -> Result<Subgraph>; // 3
    pub fn find_path(&self, from: &str, to: &str, edge_kinds: Option<&[EdgeKind]>)
        -> Result<Option<Vec<PathStep>>>;                                                       // all kinds
    pub fn get_ancestors(&self, node_id: &str) -> Result<Vec<Node>>;
    pub fn get_children(&self, node_id: &str) -> Result<Vec<Node>>;
    pub fn get_file_dependencies(&self, file_path: &str) -> Result<Vec<String>>;
    pub fn get_file_dependents(&self, file_path: &str) -> Result<Vec<String>>;
    pub fn find_circular_dependencies(&self) -> Result<Vec<Vec<String>>>;
    pub fn find_dead_code(&self, kinds: Option<&[NodeKind]>) -> Result<Vec<Node>>; // fn/method/class
    pub fn get_node_metrics(&self, node_id: &str) -> Result<NodeMetrics>;

    // Context building
    pub fn get_code(&self, node_id: &str) -> Result<Option<String>>;  // None for unknown node
    pub fn find_relevant_context(&self, query: &str, options: Option<&FindRelevantContextOptions>)
        -> Result<Subgraph>;
    pub fn build_context(&self, input: &TaskInput, options: Option<&BuildContextOptions>)
        -> Result<String>;          // formatted markdown/JSON (TS TaskContext|string collapse)
    pub fn build_task_context(&self, input: &TaskInput, options: Option<&BuildContextOptions>)
        -> Result<TaskContext>;     // structured variant for MCP
}
impl Debug for CodeGraph;           // {project_root, ..}
impl Drop for CodeGraph;            // calls close() (stops watcher, releases lock)
```

### Entry-point re-exports (mirrors the `export {…}` block of src/index.ts)

`codegraph.rs` re-exports these; `lib.rs`'s existing `pub use codegraph::*`
surfaces them at the crate root — **no lib.rs edit was needed**:

- db: `get_database_path`, `DatabaseConnection`, `QueryBuilder` (+`RoutingManifest`,
  `TopRouteFile`, `SqliteBackend` because they appear in facade signatures)
- directory: `get_codegraph_dir`, `is_initialized`, `find_nearest_codegraph_root`, `CODEGRAPH_DIR`
- extraction: `IndexProgress`, `IndexResult`, `SyncResult`, `ChangedFiles`,
  `detect_language`, `is_language_supported`, `is_grammar_loaded`,
  `get_supported_languages`, `init_grammars`, `load_grammars_for_languages`, `load_all_grammars`
- resolution: `ResolutionResult`; graph: `NodeMetrics`, `PathStep`
- errors: `set_logger`, `DefaultLogger` (lib.rs already exports `CodeGraphError`,
  `Logger`, `SilentLogger`, `Result`)
- utils: `FileLock`; sync: `FileWatcher`, `WatchOptions`, `PendingFile`, `LockUnavailableError`
- **`MCPServer` NOT re-exported** (TS index.ts exports it; the MCP port is in
  flight and `mcp/mod.rs` has no server re-export yet). MCP owner: once your
  server type lands, add the re-export in *your* mod (or ask the lib.rs owner);
  matching TS would be `pub use crate::mcp::MCPServer` at the entry.

## Deviations from TS (deliberate, all behavior-argued)

1. **Per-operation orchestrator.** TS holds one `ExtractionOrchestrator` for the
   instance lifetime; Rust `ExtractionOrchestrator<'a>` borrows the QueryBuilder,
   which would make the facade self-referential. So `index_all`/`index_files`/
   `sync`/`get_changed_files` construct one per call. Only observable effect:
   the orchestrator's lazily-detected framework-name cache is re-detected per
   operation (equal or *fresher* than TS — `index_all` resets it per run in TS
   anyway, and `sync` calls `reset_detected_frameworks()` on the per-call
   instance exactly where TS does).
2. **Watcher sync runs on the worker thread via a fresh short-lived instance.**
   The TS watcher closure called `this.sync()` on the shared instance (same
   event loop). `CodeGraph` is `!Send`, so the `SyncFn` closure captures only
   the project-root `PathBuf` and does `open_sync → sync → close` per debounced
   sync. Cross-process/cross-thread writes serialize on the lock FILE: if the
   main instance (or any process) holds it, the worker's `sync()` returns the
   zero-shape, which the closure converts to `LockUnavailableError` so the
   watcher keeps pendingFiles + reschedules (#449) — where TS would have queued
   on the in-process mutex. Net effect identical: the sync happens once the
   lock frees. WAL makes worker writes visible to the main connection.
   *Caveat:* the main instance's QueryBuilder node LRU (`get_node_by_id`) can
   serve a stale entry for nodes rewritten by a watcher-thread sync; SQL-backed
   reads (search/stats/files/edges) are always fresh. TS shared one
   QueryBuilder whose caches the pipeline invalidated. Call
   `queries.clear_cache()`-equivalent paths… not exposed on the facade; if an
   MCP consumer needs strict node-by-id freshness under watch, reopen or use
   search. (Not observed by any ported test.)
3. **`index_mutex` is `std::sync::Mutex<()>`** (TS async Mutex). The instance
   is `!Sync`, so it never contends across threads; it powers `is_indexing()`
   (a `try_lock` probe), which is observably `true` from inside progress
   callbacks — test-pinned. A re-entrant `index_all` from a progress callback
   would deadlock (TS would queue it); don't do that. Poisoning is neutralized
   (`unwrap_or_else(into_inner)`).
4. **Error plumbing:** TS methods are infallible-typed but can throw; Rust
   returns `crate::error::Result`. Lifecycle error STRINGS match TS:
   `"CodeGraph already initialized in <root>"`,
   `"CodeGraph not initialized in <root>. Run init() first."`,
   `"Invalid CodeGraph directory: <errors joined ', '>"`.
   The lock-failure RESULTS (not errors) are byte-shaped to TS:
   `index_all`/`index_files` → `success:false` + error message
   `"Could not acquire file lock - another process may be indexing"`,
   `severity:"error"`, `durationMs:0`; `sync` → all-zero shape (load-bearing
   for the watcher's #449 detection; a real empty sync has filesChecked > 0).
5. **TS default args → `Option` params** (None = TS default); defaults listed
   in the surface above and in rustdoc.
6. **`nodes_created`/`edges_created` recompute** uses `saturating_sub` (TS
   subtraction could go negative when a reindex shrinks the graph; the counts
   are usize here).
7. **`close()` takes `&self`** (RefCell interior mutability), is idempotent,
   and also runs on `Drop` (TS had no destructor; Rust consumers that forget
   close still stop the watcher thread + release the lock + close SQLite).
8. **`destroy()` is `#[deprecated]`** (mirrors the TS `@deprecated` JSDoc).
9. **`build_context` returns `Result<String>`** (context-module convention:
   the TS `TaskContext | string` raw-object path is unreachable);
   `build_task_context` exposes the struct (extra, for the MCP wave).
10. `init`/`open` are sync fns; `init_grammars()` is still called where TS
    awaited it (it's a native no-op kept for parity).
11. `path.resolve` parity via `utils::lexical_resolve(cwd, root)`.
12. `sync()`'s changed-ref dedupe replicates the TS
    `new Map([...byFile,...byName])` exactly: key
    `fromNodeId\0referenceName\0referenceKind`, first occurrence keeps its
    position, later duplicate replaces the value (unit-test pinned).
13. `wait_until_watcher_ready` (TS `waitUntilWatcherReady`): `None` timeout =
    the watcher's 10 000 ms default; returns Ok immediately with no watcher.

## Wiring contract for the MCP / CLI / installer waves

- **Construct:** `CodeGraph::init / init_sync / open / open_sync` — everything
  else hangs off the instance. Keep one instance per project per thread.
- **#238 reuse contract (MCP):** ToolHandler must reuse the default instance
  when a tool's `projectPath` resolves (via `find_nearest_codegraph_root`) to
  the default project root — do NOT `open_sync` a second connection
  (`concurrent-locking.test.ts` describe #3 is yours to port against that).
- **CLI `index`/`sync` progress:** pass `IndexOptions { on_progress: Some(&cb) }`;
  adapt `extraction::IndexProgress` → `ui::IndexProgress` at the CLI boundary
  (`phase.as_str()`, current/total as u64) per notes/ui.md. The resolving phase
  arrives as `IndexPhase::Resolving` with current/total (TS parity).
- **`codegraph status`:** `get_backend().as_str()` ("native"),
  `get_journal_mode()`, `get_stats()`, `get_last_indexed_at()` (#329).
- **MCP staleness banner (#403):** `get_pending_files()`.
- **Abort:** `IndexOptions.signal: Option<&AtomicBool>` (true = aborted).
- **installer `install.rs:629` TODO(wiring):** `CodeGraph::init(path,
  &InitOptions{ index:true, on_progress })` now exists.
- `uninitialize()` for CLI `uninit` (plus `sync::remove_git_sync_hook` per
  notes/sync.md).

## Test port map (tests/codegraph_api_test.rs — 42 tests)

- `sync.test.ts` (ALL 20 cases — the whole file was deferred to this wave):
  - "Sync Functionality" (10): getChangedFiles add/modify/remove; sync
    reindexes added/modified/deleted; no-op sync filesChecked>0; indexAll
    reconciles deletions; unresolved refs repairable across indexAll runs
    (incl. the double-indexAll wipe regression); search pagination after
    final scoring (130 vendor + 8 focused fixture).
  - "Git-based sync" (10): modified/untracked/deleted via git (+
    changedFilePaths assertions); untracked-once-indexed #206; untracked
    re-index on change; too-large tracked file purge (2 MiB > the 1 MiB cap);
    late target resolves existing unresolved refs; unsupported extensions
    skipped; clean-tree no-op (changedFilePaths None); checkout-with-clean-
    status detection. Real `git` in tempdirs (same helper as sync_test.rs).
- `concurrent-locking.test.ts` (#238): bounded busy_timeout; WAL mode via
  `get_journal_mode`; reader-proceeds-during-`BEGIN EXCLUSIVE` writer (with
  the TS non-WAL skip). The ToolHandler describe is the **MCP wave's** (needs
  ToolHandler + the instance-reuse spy). Added two facade-level lock tests TS
  covers implicitly: live-PID foreign lock → exact lock-failure shapes for
  indexAll AND sync (#449 zero-shape), foreign lock not deleted, recovery
  after release; stale dead-PID lock takeover through `index_all`.
- `security.test.ts` "Path Traversal Prevention" (2): getCode valid node /
  unknown node → None. (validateProjectPath/MCP-validation/symlink suites
  were already ported by their owning waves per notes/ui.md.)
- `foundation.test.ts` facade cases deferred by notes/ui.md (9): openSync
  /not initialized/i; double-init /already initialized/i; reopen returns
  working instance + project root; getStats counts + dbSizeBytes + optimize +
  clear; backend/journal/lastIndexedAt; destroy keeps `.codegraph/`;
  uninitialize removes it; getContext /Node not found/ + traverse/callGraph/
  typeHierarchy/findUsages empty on unknown ids; is_indexing probe.
- `watcher.test.ts` "CodeGraph integration" (3): watch/unwatch via API
  (inert); watcher stops on close; REAL end-to-end watcher test (real write →
  OS events → debounce → worker-thread sync → getStats/searchNodes), with a
  policy-disabled early-return guard like the env-gated runners.
- Deferred e2e absorbed from other waves: extraction.test.ts "IDA sub
  callers/callees after indexAll" (notes/extraction-orchestrator.md);
  object-literal-methods.test.ts "(end-to-end)" store-action callers
  (notes/resolution-stitch.md).
- Smoke test: 2-file TS fixture; counts stable across re-index (no node/edge
  explosion), callers/callees resolve across the import, impact radius +
  find_path. NOTE: `get_file_dependencies` on a plain TS import fixture
  returns `[]` in BOTH implementations (the extraction-time `imports` edge
  targets the *import node*, which lives in the importing file; file→file
  import edges come from other resolution paths, e.g. C/C++ includes) — don't
  "fix" the Rust side alone.

## Left for later waves

- `concurrent-locking.test.ts` describe 3 (ToolHandler reuse + concurrent
  tool calls) → MCP wave.
- `worktree-detection.test.ts` describe 2 (mismatch on hot read tools +
  detection caching) → MCP wave (per notes/sync.md).
- TS `index.ts` re-export of `MCPServer` → MCP owner (see above).

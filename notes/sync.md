# sync module port notes

Port of `src/sync/{index,watcher,watch-policy,git-hooks,worktree}.ts` →
`rust/src/sync/{mod,watcher,watch_policy,git_hooks,worktree}.rs`.
Tests: `rust/tests/sync_test.rs` (26), `rust/tests/git_hooks_test.rs` (7),
plus 20 in-module unit tests. All pass; `cargo check --all-targets` clean.

## Public API (what the wiring waves consume)

### watcher.rs (TS `watcher.ts`)

```rust
pub const DEFAULT_DEBOUNCE_MS: u64 = 2000;          // TS: options.debounceMs ?? 2000
pub const DEFAULT_READY_TIMEOUT_MS: u64 = 10_000;   // TS waitUntilReady default

#[serde(rename_all = "camelCase")]
pub struct WatchSyncResult { pub files_changed: usize, pub duration_ms: i64 }
    // TS anonymous `{ filesChanged, durationMs }` a syncFn resolves with

pub type SyncError = Box<dyn std::error::Error + Send + Sync>;
pub type SyncFn = Arc<dyn Fn() -> Result<WatchSyncResult, SyncError> + Send + Sync>;

pub struct LockUnavailableError { pub message: String }   // impl Error + Display
    // LockUnavailableError::DEFAULT_MESSAGE ==
    // "CodeGraph file lock unavailable; another process is writing"
    // Signal it by returning Err(Box::new(LockUnavailableError::new())) from a
    // sync_fn; the watcher DOWNCASTS to detect it (Rust analog of instanceof).

#[serde(rename_all = "camelCase")]
pub struct PendingFile { pub path: String, pub first_seen_ms: i64,
                         pub last_seen_ms: i64, pub indexing: bool }

#[derive(Clone, Default)]
pub struct WatchOptions {
    pub debounce_ms: Option<u64>,
    pub on_sync_complete: Option<Arc<dyn Fn(WatchSyncResult) + Send + Sync>>,
    pub on_sync_error: Option<Arc<dyn Fn(&SyncError) + Send + Sync>>,
    pub inert_for_tests: bool,
}

pub struct FileWatcher;   // all methods &self (interior mutability, Arc-friendly)
impl FileWatcher {
    pub fn new(project_root: impl Into<PathBuf>, sync_fn: SyncFn, options: WatchOptions) -> Self;
    pub fn start(&self) -> bool;
    pub fn stop(&self);                       // idempotent; also runs on Drop
    pub fn is_active(&self) -> bool;
    pub fn wait_until_ready(&self, timeout_ms: u64) -> crate::error::Result<()>;
    pub fn get_pending_files(&self) -> Vec<PendingFile>;
    pub fn ingest_event_for_tests(&self, rel_path: &str);    // TS ingestEventForTests
}

pub fn emit_watch_event_for_tests(project_root: &str, rel_path: &str) -> bool;
    // TS __emitWatchEventForTests. Registry only populated when
    // IS_TEST_RUNTIME (env VITEST non-empty, or NODE_ENV == "test") — checked
    // at start()/stop() time. Registry holds Weak refs; key is the
    // project_root string exactly as passed to FileWatcher::new.
```

### watch_policy.rs (TS `watch-policy.ts`) — was already ported; reviewed, no fixes needed

```rust
pub fn detect_wsl() -> bool;                                  // cached
pub struct WatchProbe { pub env: Option<HashMap<String,String>>, pub is_wsl: Option<bool> }
pub fn watch_disabled_reason(project_root: &str, probe: &WatchProbe) -> Option<String>;
pub fn reset_wsl_cache_for_tests();                           // TS __resetWslCacheForTests
```
Production callers pass `&WatchProbe::default()` (process env + auto WSL
detect) — that is what `FileWatcher::start()` does internally.

### git_hooks.rs (TS `git-hooks.ts`) — was already ported; reviewed, no fixes needed

```rust
pub enum GitHookName { PostCommit, PostMerge, PostCheckout }  // as_str() → "post-commit"…
                                                              // serde renames match the TS strings
pub const DEFAULT_SYNC_HOOKS: [GitHookName; 3];
#[serde(rename_all = "camelCase")]
pub struct GitHookResult { pub installed: Vec<GitHookName>,
                           pub hooks_dir: Option<PathBuf>, pub skipped: Option<String> }
pub fn is_git_repo(project_root: &Path) -> bool;
pub fn install_git_sync_hook(project_root: &Path, hooks: &[GitHookName]) -> GitHookResult;
pub fn remove_git_sync_hook(project_root: &Path, hooks: &[GitHookName]) -> GitHookResult;
pub fn is_sync_hook_installed(project_root: &Path, hooks: &[GitHookName]) -> bool;
pub(crate) fn git_output(args: &[&str], cwd: &Path) -> Option<String>;  // shared w/ worktree.rs
```
TS default-arg `hooks = DEFAULT_SYNC_HOOKS` → callers pass `&DEFAULT_SYNC_HOOKS`
explicitly. **Hook script bytes are byte-identical to TS** (markers, comments,
`command -v codegraph` guard, two-space indent) — locked by the unit test
`marker_block_bytes_match_the_ts_snippet`, so hooks installed by the TS build
round-trip through the Rust install/remove.

### worktree.rs (TS `worktree.ts`)

```rust
pub fn git_worktree_root(dir: &Path) -> Option<PathBuf>;      // realpath'd toplevel
#[serde(rename_all = "camelCase")]
pub struct WorktreeIndexMismatch { pub worktree_root: PathBuf, pub index_root: PathBuf }
pub fn detect_worktree_index_mismatch(start_path: &Path, index_root: &Path)
    -> Option<WorktreeIndexMismatch>;
pub fn worktree_mismatch_warning(m: &WorktreeIndexMismatch) -> String;  // multi-line, status
pub fn worktree_mismatch_notice(m: &WorktreeIndexMismatch) -> String;   // one-line, read tools
```
User-visible strings match TS verbatim (including the `⚠` prefix and
`codegraph init -i` fix instruction).

`mod.rs` re-exports all of the above (mirrors `src/sync/index.ts`, plus
`WatchProbe`/`WatchSyncResult`/`SyncFn`/`SyncError`, which Rust signatures need).

## What CodeGraph::watch / MCP / installer need (wiring contract)

- **`CodeGraph::watch(options) -> bool`** (TS `src/index.ts:522`): construct
  `FileWatcher::new(project_root, sync_fn, options)` where `sync_fn` wraps
  `self.sync()` and converts the lock-failure zero-shape into the typed error
  (#449):
  ```rust
  // sync() returns the exact zero-shape iff the file lock couldn't be
  // acquired (a real empty sync always has files_checked > 0).
  if result.files_checked == 0 && result.duration_ms == 0 {
      return Err(Box::new(LockUnavailableError::new()));
  }
  Ok(WatchSyncResult {
      files_changed: result.files_added + result.files_modified + result.files_removed,
      duration_ms: result.duration_ms,
  })
  ```
  Then `watcher.start()`. Also: `unwatch()` → `stop()`, `is_watching()` →
  `is_active()`, `get_pending_files()` (empty vec when no watcher),
  `wait_for_watcher_ready(ms)` → `wait_until_ready(ms)`, and `close()` must
  call `unwatch()`. `CodeGraph` should keep `Option<Arc<FileWatcher>>` (or rely
  on Drop).
- **MCP `tools.rs`**: needs `PendingFile` (staleness banner, #403) and
  `worktree_mismatch_notice`/`worktree_mismatch_warning` +
  `detect_worktree_index_mismatch` (#155). NOTE: the once-per-session
  *caching* of the detection (so later tool calls spawn no git) lives in the
  TS ToolHandler, not in this module — the MCP port must replicate it.
- **MCP `engine.rs`**: `watch_disabled_reason` for diagnostics (already
  referenced by a NOT-YET-PORTED comment in `installer/install.rs:636`).
- **Installer**: `watch_disabled_reason`, `is_git_repo`,
  `is_sync_hook_installed`, `install_git_sync_hook` (and `remove_git_sync_hook`
  from the CLI `uninit` path).

## Env vars (exact TS names/semantics preserved)

- `CODEGRAPH_NO_WATCH=1` — watcher off (wins over everything).
- `CODEGRAPH_FORCE_WATCH=1` — overrides WSL auto-detect.
- `CODEGRAPH_MAX_DIR_WATCHES` — Linux per-directory watch cap (default 50_000).
- `WSL_DISTRO_NAME` / `WSL_INTEROP` + `/proc/version` — WSL detection.
- `VITEST` / `NODE_ENV=test` — gates the test-seam registry.

## Deviations from TS (all behavior-reviewed)

1. **notify v8 instead of `fs.watch`**, same per-platform strategy: one
   recursive watch on macOS/Windows; on Linux explicitly NON-recursive
   per-directory inotify watches built by our own ignore-aware tree walk
   (notify's recursive mode would descend into node_modules and blow the
   inotify budget — the exact thing the TS design avoids, #579).
2. **No async runtime**: one worker `std::thread` per started watcher owns the
   notify watcher + a crossbeam channel; the debounce is a deadline serviced
   via `recv_timeout` (TS `setTimeout`). `start()` blocks on a rendezvous
   until the watch set is installed, so it stays synchronous like TS.
3. `sync_fn` runs synchronously **on the worker thread**. OS events arriving
   mid-sync queue in the channel and enter `pending_files` only after
   `sync_fn` returns (TS updated them live on the event loop). Net pruning
   semantics are identical (their `last_seen_ms` ends up > `sync_started_ms`,
   so they survive into the rescheduled follow-up sync); the only observable
   difference is `get_pending_files()` during an in-flight sync not yet
   listing mid-sync edits (`indexing:false` entries in TS).
4. `get_pending_files()` order is path-sorted (BTreeMap) instead of TS Map
   insertion order. Content identical; deterministic.
5. `LockUnavailableError` detection is a downcast on
   `Box<dyn Error + Send + Sync>` (TS `instanceof`).
6. `stop()` joins the worker thread (skipped when called from a watcher
   callback to avoid self-join); TS only closed handles. `Drop` calls `stop()`.
7. Faithful TS quirk kept: on the Linux per-directory path a root-watch
   failure makes `start()` return **true** with `is_active() == false`
   (TS `watchTree` swallows the error); a recursive-watch failure returns
   false. One divergence: TS `isActive()` recomputes from the live watcher
   map (flips false if every per-dir watch later errors away); Rust caches the
   start-time truth until `stop()`.
8. git_hooks: a failed write/read of an individual hook file skips that hook
   (it's omitted from `installed`) instead of throwing out of the whole
   install like TS `writeFileSync` would. Signature stays non-Result.
9. worktree `realpath`: `std::path::absolute` + `canonicalize` fallback.
   For *nonexistent* paths containing `..`, `..` is not collapsed (TS
   `path.resolve` collapses); irrelevant for existing paths. On Windows,
   canonicalize yields `\\?\`-prefixed paths — equality is internally
   consistent, but warning strings would show the prefix (revisit in the
   Windows-validation pass).
10. `is_test_runtime()` is evaluated at `start()`/`stop()` time, not cached at
    module load (lets Rust tests opt in via `NODE_ENV=test`).
11. `CODEGRAPH_MAX_DIR_WATCHES` values that overflow `usize` fall back to the
    default (TS accepted any `^\d+$` as a float).
12. `watch_disabled_reason` takes `&WatchProbe` (no default args in Rust);
    `WatchProbe::default()` == TS `{}`.

## Test mapping

- `__tests__/watcher.test.ts` → `tests/sync_test.rs` (inert mode +
  `ingest_event_for_tests`, real debounce deadlines, generous `wait_for`
  margins). The TS file's one end-to-end fs.watch test is covered by THREE
  non-inert real-watcher tests (real write → sync; dir-created-after-start →
  sync, exercising the Linux `mark_existing` race closure; node_modules churn
  → no sync) that don't need the CodeGraph API.
- `__tests__/watch-policy.test.ts` → pure cases as unit tests in
  `watch_policy.rs` (pre-existing, verified complete); the
  "FileWatcher honors the policy" case → `does_not_start_when_codegraph_no_watch_is_set`
  in `sync_test.rs`. Env mutation is serialized via an ENV_LOCK RwLock
  (writers mutate env; every `start()`-calling test holds a read lock) since
  Rust integration tests share one process, unlike vitest.
- `__tests__/git-hooks.test.ts` → `tests/git_hooks_test.rs`, all 7 cases,
  real `git` in temp dirs, executable-bit assertions `#[cfg(unix)]`-gated
  (always-true on Windows, like TS).
- `__tests__/worktree-detection.test.ts` (first describe) →
  `sync_test.rs::worktree::*`, all 7 cases, real `git worktree add`.

## Deferred to the integration wave (need CodeGraph / MCP ports)

- **All of `__tests__/sync.test.ts`** (needs `CodeGraph::init_sync`,
  `index_all`, `sync`, `get_changed_files`, `search_nodes`, `get_callers`):
  getChangedFiles add/modify/remove; sync reindexes added/modified/removed;
  no-op sync reports filesChecked > 0; indexAll reconciles deletions; unresolved
  refs repairable across indexAll runs; search pagination after scoring;
  git-based sync (modified/untracked/deleted via git, untracked-once-indexed
  #206, untracked re-index on change, too-large tracked file purge, late
  target resolves existing unresolved refs, unsupported extensions skipped,
  clean-tree no-op, checkout-with-clean-status detection).
- `watcher.test.ts` "CodeGraph integration" describe: watch/unwatch via
  CodeGraph API, watcher stops on `close()`, real end-to-end
  write→watch→sync→graph (`getStats`/`searchNodes` assertions).
- `worktree-detection.test.ts` second describe ("mismatch surfaces on hot
  read tools"): needs ToolHandler — includes the detection-result caching
  contract (later tool call must not spawn git).
- `__tests__/is-test-file.test.ts` is **not** a sync test: it targets
  `isTestFile` in `src/search/query-utils.ts` → belongs to the search module
  port (not covered here; flagged to the search owner via this note).

//! File Watcher
//!
//! Watches the project directory for file changes and triggers debounced sync
//! operations to keep the code graph up-to-date.
//!
//! Port of `src/sync/watcher.ts`. The TS implementation uses Node's built-in
//! `fs.watch`; this port uses the `notify` crate (v8) with the SAME
//! per-platform strategy, chosen to keep the open-descriptor / kernel-watch
//! cost BOUNDED rather than growing with the number of files:
//!
//!   - macOS / Windows: a SINGLE recursive watch on the project root.
//!     notify maps this to one FSEvents stream (macOS) / one
//!     ReadDirectoryChangesW handle (Windows), so it costs O(1) descriptors no
//!     matter how large the tree. This is the fix for the macOS file-table
//!     exhaustion (#644 / #496 / #555 / #628): the previous watcher held one
//!     open fd PER WATCHED FILE on macOS (tens of thousands of REG fds), which
//!     exhausted `kern.maxfiles` and crashed unrelated processes system-wide.
//!
//!   - Linux: we deliberately do NOT hand notify a recursive watch (its inotify
//!     backend would walk into node_modules/ etc.). Instead we watch each
//!     (non-ignored) DIRECTORY with one non-recursive inotify watch —
//!     O(directories), NOT O(files). New directories are picked up dynamically
//!     and an overall watch cap bounds inotify usage on pathological monorepos
//!     (#579). A single inotify watch on a directory already reports
//!     create/modify/delete for its children, so per-file watches are never
//!     needed.
//!
//! Excluded trees (node_modules/, dist/, .git/, …) are filtered via the
//! indexer's `build_default_ignore` (built-in default-ignore dirs + the
//! project's .gitignore) — on Linux they're never descended into (so they cost
//! no watch), and on macOS/Windows the single recursive stream still covers
//! them but their events are dropped before any sync is scheduled. Either way
//! the watcher's scope matches the indexer's (#276 / #407).
//!
//! Threading model (no async runtime, per rust/PORTING.md): one worker thread
//! per started watcher owns the notify watcher and a crossbeam channel. The
//! worker implements the debounce (TS `setTimeout`) as a deadline checked via
//! `recv_timeout`, and runs `sync_fn` inline when it fires.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded, unbounded};
use ignore::gitignore::Gitignore;
use notify::event::{AccessKind, AccessMode, EventKind, MetadataKind, ModifyKind};
use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};
use serde::Serialize;

use crate::directory::is_codegraph_data_dir;
use crate::error::{log_debug, log_warn};
use crate::extraction::{build_default_ignore, is_source_file_with_overrides};
use crate::project_config::{
    PROJECT_CONFIG_FILENAME,
    ProjectConfig,
    load_project_config,
    matcher_matches,
};
use crate::sync::watch_policy::{WatchProbe, watch_disabled_reason};
use crate::utils::normalize_path;

/// Native recursive watching is only reliable (and O(1)) on macOS and Windows;
/// on Linux notify's inotify backend would emulate recursion by walking every
/// directory — including ignored trees — so we branch to the per-directory
/// strategy there instead. (TS: `supportsRecursiveWatch()`.)
fn supports_recursive_watch() -> bool {
    cfg!(target_os = "macos") || cfg!(target_os = "windows")
}

/// Upper bound on simultaneously-watched directories on the Linux per-directory
/// path. Each is one inotify watch; the kernel's `fs.inotify.max_user_watches`
/// is the hard limit (commonly 8k–128k). We stop adding watches past this and
/// log once — partial live-watch (with `codegraph sync` as the backstop) is far
/// better than exhausting the user's inotify budget and breaking watching
/// system-wide (#579). Tunable via CODEGRAPH_MAX_DIR_WATCHES.
const DEFAULT_MAX_DIR_WATCHES: usize = 50_000;

fn max_dir_watches() -> usize {
    if let Ok(raw) = std::env::var("CODEGRAPH_MAX_DIR_WATCHES") {
        if !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit()) {
            if let Ok(n) = raw.parse::<usize>() {
                if n > 0 {
                    return n;
                }
            }
        }
    }
    DEFAULT_MAX_DIR_WATCHES
}

/// Default debounce delay (TS: `options.debounceMs ?? 2000`).
pub const DEFAULT_DEBOUNCE_MS: u64 = 2000;

/// Default timeout for [`FileWatcher::wait_until_ready`] (TS default: 10000ms).
pub const DEFAULT_READY_TIMEOUT_MS: u64 = 10_000;

/// Result shape a `sync_fn` resolves with — the anonymous
/// `{ filesChanged, durationMs }` object in TS (`CodeGraph.watch` adapts
/// `SyncResult` into this).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchSyncResult {
    pub files_changed: usize,
    pub duration_ms: i64,
}

/// Error type a `sync_fn` may fail with. Use a boxed
/// [`LockUnavailableError`] to signal the lock-held no-op case — the watcher
/// downcasts to detect it (the Rust equivalent of the TS `instanceof` check).
pub type SyncError = Box<dyn std::error::Error + Send + Sync>;

/// The debounced sync callback (TS: `() => Promise<{filesChanged, durationMs}>`).
pub type SyncFn = Arc<dyn Fn() -> Result<WatchSyncResult, SyncError> + Send + Sync>;

/// Diagnostics callback invoked when a sync errors (TS: `onSyncError`).
pub type SyncErrorHandler = Arc<dyn Fn(&SyncError) + Send + Sync>;

/// Returned (boxed) by a `sync_fn` to signal that the underlying sync couldn't
/// acquire the cross-process write lock (#449). The watcher treats this as "no
/// progress" — preserves `pending_files`, skips `on_sync_complete`, and the
/// `finally` path reschedules. Quiet (debug-only) because a long-running
/// external indexer can hit this every debounce cycle.
#[derive(Debug, Clone)]
pub struct LockUnavailableError {
    pub message: String,
}

impl LockUnavailableError {
    /// The TS constructor's default message.
    pub const DEFAULT_MESSAGE: &'static str =
        "CodeGraph file lock unavailable; another process is writing";

    pub fn new() -> Self {
        Self {
            message: Self::DEFAULT_MESSAGE.to_string(),
        }
    }

    pub fn with_message(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Default for LockUnavailableError {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for LockUnavailableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for LockUnavailableError {}

/// Per-file pending entry — tracks a source file the watcher saw an event for
/// but hasn't yet synced into the index. Exposed via
/// [`FileWatcher::get_pending_files`] so MCP tool responses can mark stale
/// results without forcing a wait.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingFile {
    /// Project-relative POSIX path (e.g. "src/foo.ts").
    pub path: String,
    /// Wall-clock ms at the first event we saw for this path since the last sync.
    pub first_seen_ms: i64,
    /// Wall-clock ms at the most recent event we saw for this path.
    pub last_seen_ms: i64,
    /// True when a sync is currently in flight that began AFTER this file's most
    /// recent event — i.e. the next successful sync will pick it up. False when
    /// the file is still in the debounce window (no sync running yet).
    pub indexing: bool,
}

/// Options for the file watcher (TS: `WatchOptions`).
#[derive(Clone, Default)]
pub struct WatchOptions {
    /// Debounce delay in milliseconds.
    /// After the last file change, wait this long before triggering sync.
    /// Default: 2000ms.
    pub debounce_ms: Option<u64>,

    /// Callback when a sync completes (for logging/diagnostics).
    pub on_sync_complete: Option<Arc<dyn Fn(WatchSyncResult) + Send + Sync>>,

    /// Callback when a sync errors (for logging/diagnostics).
    pub on_sync_error: Option<SyncErrorHandler>,

    /// Test-only. When true, `start()` installs NO OS-level watcher — the
    /// watcher is "inert" and only the [`emit_watch_event_for_tests`] /
    /// [`FileWatcher::ingest_event_for_tests`] seam drives its pipeline. This
    /// keeps unit tests deterministic and OS-free (real FSEvents/inotify
    /// delivery races under parallel test runners). Production never sets it.
    pub inert_for_tests: bool,
}

#[derive(Clone, Copy)]
struct PendingInfo {
    first_seen_ms: i64,
    last_seen_ms: i64,
}

/// Mutable watcher state, shared between the public handle, the worker thread,
/// and the test seam.
struct WatcherState {
    /// Equivalent of TS `recursiveWatcher !== null || dirWatchers.size > 0 || inert`.
    active: bool,
    /// Test-only inert mode: started, but with no OS watcher installed.
    inert: bool,
    stopped: bool,
    /// True once the initial watch set is established. There is no
    /// asynchronous initial "crawl" — this flips to true synchronously at the
    /// end of `start()`. The startup reconcile against on-disk state is the
    /// engine's catch-up sync, not the watcher's job.
    ready: bool,
    syncing: bool,
    /// Wall-clock ms at which the in-flight sync began. Combined with
    /// `pending_files`'s `last_seen_ms`, this distinguishes "still in the
    /// debounce window" from "currently being indexed".
    sync_started_ms: i64,
    /// The debounce deadline (TS `setTimeout` handle equivalent). `None` means
    /// no sync is scheduled.
    deadline: Option<Instant>,
    /// Files seen by the watcher since the last successful sync — populated on
    /// every change event, pruned only after a sync commits successfully (and
    /// only for entries whose `last_seen_ms <= sync_started_ms`). Keyed by the
    /// same project-relative POSIX path the rest of the codebase uses.
    /// (BTreeMap so `get_pending_files()` output order is deterministic.)
    pending_files: BTreeMap<String, PendingInfo>,
    /// The shared ignore matcher (built-in defaults + project .gitignore),
    /// built once at `start()`. Same source of truth the indexer uses, so
    /// watcher scope can never diverge from index scope.
    ignore: Option<Arc<Gitignore>>,
}

/// Everything the worker thread and the public handle share.
struct WatcherShared {
    project_root: PathBuf,
    /// `project_root` as given (string form) — registry key + policy input.
    project_root_str: String,
    debounce_ms: u64,
    sync_fn: SyncFn,
    on_sync_complete: Option<Arc<dyn Fn(WatchSyncResult) + Send + Sync>>,
    on_sync_error: Option<SyncErrorHandler>,
    inert_for_tests: bool,
    state: Mutex<WatcherState>,
    ready_cv: Condvar,
    /// Sender into the live worker (None when not started). Used by the test
    /// seam to wake the worker after mutating the deadline.
    tx: Mutex<Option<Sender<WorkerMsg>>>,
}

enum WorkerMsg {
    /// A raw notify event (or backend error).
    Fs(notify::Result<notify::Event>),
    /// Wake the worker so it recomputes its debounce deadline (test seam).
    Kick,
    Stop,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Our own dirs are always ignored, regardless of .gitignore.
fn is_always_ignored(rel: &str) -> bool {
    let first = rel.split('/').next().unwrap_or(rel);
    is_codegraph_data_dir(first) || first == ".git"
}

/// npm `ignore`-pkg `.ignores()` parity (same as the orchestrator's private
/// helper): a path is ignored when the path or any of its ancestor directories
/// matches an ignore rule (negations re-include).
fn gitignore_ignores(ig: &Gitignore, rel_path: &str, is_dir: bool) -> bool {
    if rel_path.is_empty() {
        return false;
    }
    ig.matched_path_or_any_parents(rel_path, is_dir).is_ignore()
}

/// Project-relative POSIX path of `p` under `root`, or `None` when `p` IS the
/// root or lies outside it (the TS code's `!rel || rel === '.' ||
/// rel.startsWith('..')` guard).
fn rel_to_root(root: &Path, p: &Path) -> Option<String> {
    let rp = p.strip_prefix(root).ok()?;
    let s = rp.to_string_lossy();
    if s.is_empty() || s == "." {
        return None;
    }
    Some(normalize_path(&s))
}

fn is_index_relevant_event(kind: &EventKind) -> bool {
    match *kind {
        EventKind::Any | EventKind::Create(_) | EventKind::Remove(_) => true,
        EventKind::Modify(ModifyKind::Metadata(MetadataKind::AccessTime)) => false,
        EventKind::Modify(_) => true,
        EventKind::Access(AccessKind::Open(AccessMode::Write))
        | EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
        EventKind::Access(_) | EventKind::Other => false,
    }
}

/// Shared change handler for both watch strategies (TS: `handleChange`). `rel`
/// is a project-relative POSIX path. Applies the ignore + source-file filters
/// and, for a real source change, records it as pending (#403) and schedules a
/// debounced sync (sets the deadline; the worker picks it up).
///
/// The recursive (macOS/Windows) watcher reports events for ignored trees too
/// (one stream covers the whole repo), so the ignore check here is load-bearing
/// — it drops node_modules/dist/.git churn before any sync is scheduled.
fn handle_change(shared: &WatcherShared, rel: &str) {
    if rel.is_empty() || rel == "." || rel.starts_with("..") {
        return;
    }
    if is_always_ignored(rel) {
        return;
    }
    let config = load_project_config(&shared.project_root);
    if matcher_matches(
        config.exclude_matcher(&shared.project_root).as_ref(),
        rel,
        false,
    ) {
        return;
    }
    let explicitly_included = matcher_matches(
        config.include_matcher(&shared.project_root).as_ref(),
        rel,
        false,
    );
    {
        let st = shared.state.lock().unwrap();
        if let Some(ig) = st.ignore.as_ref() {
            if gitignore_ignores(ig, rel, false) && !explicitly_included {
                return;
            }
        }
    }
    if rel != PROJECT_CONFIG_FILENAME
        && !is_source_file_with_overrides(rel, config.extension_overrides())
    {
        return;
    }

    log_debug(
        "File change detected",
        Some(&serde_json::json!({ "file": rel })),
    );
    let mut st = shared.state.lock().unwrap();
    if st.ready {
        let now = now_ms();
        let entry = st
            .pending_files
            .entry(rel.to_string())
            .or_insert(PendingInfo {
                first_seen_ms: now,
                last_seen_ms: now,
            });
        entry.last_seen_ms = now;
    }
    // scheduleSync(): each change resets the debounce timer.
    st.deadline = Some(Instant::now() + Duration::from_millis(shared.debounce_ms));
}

/// True for any directory that should NOT be watched (used while building the
/// Linux per-directory watch tree). Tests the directory form of the path so a
/// dir-only ignore rule like `build/` matches.
fn should_ignore_dir(shared: &WatcherShared, dir_path: &Path) -> bool {
    let Some(rel) = rel_to_root(&shared.project_root, dir_path) else {
        return false; // root / outside
    };
    if is_always_ignored(&rel) {
        return true;
    }
    let config = load_project_config(&shared.project_root);
    if matcher_matches(
        config.exclude_matcher(&shared.project_root).as_ref(),
        &rel,
        true,
    ) {
        return true;
    }
    if matcher_matches(
        config.include_matcher(&shared.project_root).as_ref(),
        &rel,
        true,
    ) || include_may_match_below(&config, &rel)
    {
        return false;
    }
    let st = shared.state.lock().unwrap();
    match st.ignore.as_ref() {
        Some(ig) => gitignore_ignores(ig, &rel, true),
        None => false,
    }
}

fn include_may_match_below(config: &ProjectConfig, rel: &str) -> bool {
    let rel = rel.trim_matches('/');
    config.include.iter().any(|pattern| {
        let pattern = pattern.trim_start_matches('/');
        let prefix = pattern
            .split(['*', '?', '[', ']', '{', '}', '!'])
            .next()
            .unwrap_or("")
            .trim_matches('/');
        !prefix.is_empty()
            && (prefix == rel
                || prefix.starts_with(&format!("{rel}/"))
                || rel.starts_with(&format!("{prefix}/")))
    })
}

// =============================================================================
// Test seam registry
// =============================================================================

/// Test seam (see [`emit_watch_event_for_tests`]). Maps a watcher's project
/// root to its live shared state so tests can synthesize a change event
/// deterministically — real watch delivery latency races under parallel test
/// runners. Only populated under a test runtime, so production carries no
/// bookkeeping (entries are `Weak`, so nothing is retained either way).
fn live_watchers_for_tests()
-> &'static Mutex<std::collections::HashMap<String, Weak<WatcherShared>>> {
    static REGISTRY: OnceLock<Mutex<std::collections::HashMap<String, Weak<WatcherShared>>>> =
        OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// TS: `IS_TEST_RUNTIME = !!(process.env.VITEST || process.env.NODE_ENV === 'test')`.
/// Checked at start()/stop() time (not cached) so Rust tests can opt in by
/// setting `NODE_ENV=test` before starting a watcher.
fn is_test_runtime() -> bool {
    std::env::var("VITEST")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
        || std::env::var("NODE_ENV").as_deref() == Ok("test")
}

/// Test-only: synthesize a source-file change for the live watcher running at
/// `project_root`, exercising the real filter → pending_files → debounced-sync
/// logic without depending on OS watch delivery timing. `rel_path` is
/// project-relative POSIX (e.g. "src/foo.ts"). Returns false if no live
/// watcher is registered for that root (e.g. outside a test runtime, where the
/// registry is intentionally not populated).
///
/// (TS: `__emitWatchEventForTests`.)
pub fn emit_watch_event_for_tests(project_root: &str, rel_path: &str) -> bool {
    let shared = {
        let registry = live_watchers_for_tests().lock().unwrap();
        registry.get(project_root).and_then(|w| w.upgrade())
    };
    match shared {
        Some(shared) => {
            handle_change(&shared, &normalize_path(rel_path));
            kick(&shared);
            true
        }
        None => false,
    }
}

/// Wake the worker so it notices a deadline set from outside its own loop.
fn kick(shared: &WatcherShared) {
    if let Some(tx) = shared.tx.lock().unwrap().as_ref() {
        let _ = tx.send(WorkerMsg::Kick);
    }
}

// =============================================================================
// Worker thread
// =============================================================================

struct SetupInfo {
    mode: &'static str,
    watched_dirs: usize,
    /// TS `isActive()` truth at start: recursive watcher installed, at least
    /// one per-directory watch, or inert mode.
    active: bool,
}

struct Worker {
    shared: Arc<WatcherShared>,
    rx: Receiver<WorkerMsg>,
    /// Clone handed to the notify event handler (events loop back into `rx`).
    tx: Sender<WorkerMsg>,
    watcher: Option<RecommendedWatcher>,
    /// Linux: the set of per-directory watches (keyed by absolute path).
    watched_dirs: HashSet<PathBuf>,
    per_directory: bool,
    /// Set once the per-directory watch cap is hit, so we log only once.
    dir_cap_warned: bool,
}

impl Worker {
    /// Install the OS watch set (TS: the body of `start()`'s try block before
    /// the ready flip). Runs on the worker thread; `start()` blocks on the
    /// returned result so the caller-visible behavior stays synchronous.
    fn setup(&mut self) -> notify::Result<SetupInfo> {
        if self.shared.inert_for_tests {
            // Test-only: install no OS watcher; the seam drives events instead.
            return Ok(SetupInfo {
                mode: "inert",
                watched_dirs: 0,
                active: true,
            });
        }

        let tx = self.tx.clone();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let _ = tx.send(WorkerMsg::Fs(res));
        })?;
        self.watcher = Some(watcher);

        if supports_recursive_watch() {
            // macOS/Windows: one recursive watcher for the whole tree. O(1)
            // descriptors. A failure here fails start() (TS: fs.watch throws).
            let root = self.shared.project_root.clone();
            self.watcher
                .as_mut()
                .unwrap()
                .watch(&root, RecursiveMode::Recursive)?;
            Ok(SetupInfo {
                mode: "recursive",
                watched_dirs: 0,
                active: true,
            })
        } else {
            // Linux: walk the (non-ignored) tree and watch each directory. One
            // inotify watch per directory reports create/modify/delete for that
            // directory's direct children, so we never watch individual files.
            // Per-directory errors are skipped quietly (TS: watchTree catches),
            // so setup itself cannot fail here.
            let root = self.shared.project_root.clone();
            self.watch_tree(&root, /* mark_existing */ false);
            Ok(SetupInfo {
                mode: "per-directory",
                watched_dirs: self.watched_dirs.len(),
                active: !self.watched_dirs.is_empty(),
            })
        }
    }

    /// Add an inotify watch for `dir` and recurse into its non-ignored
    /// subdirectories. When `mark_existing` is true (a directory that appeared
    /// AFTER startup), the source files already inside it are recorded as
    /// pending — this closes the `mkdir + write` race where files created
    /// before the new directory's watch is installed would otherwise be missed
    /// until the next full sync. The initial startup walk passes false (the
    /// engine's catch-up sync owns the baseline).
    fn watch_tree(&mut self, dir: &Path, mark_existing: bool) {
        if self.watched_dirs.contains(dir) {
            return;
        }
        if self.watched_dirs.len() >= max_dir_watches() {
            if !self.dir_cap_warned {
                self.dir_cap_warned = true;
                log_warn(
                    "File watcher hit directory-watch cap; remaining subtrees rely on manual/periodic sync",
                    Some(&serde_json::json!({ "cap": max_dir_watches() })),
                );
            }
            return;
        }

        let Some(watcher) = self.watcher.as_mut() else {
            return;
        };
        // ENOENT / EACCES / too-many-open-files — skip this directory quietly.
        if watcher.watch(dir, RecursiveMode::NonRecursive).is_err() {
            return;
        }
        self.watched_dirs.insert(dir.to_path_buf());

        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let child = dir.join(entry.file_name());
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                if should_ignore_dir(&self.shared, &child) {
                    continue;
                }
                self.watch_tree(&child, mark_existing);
            } else if mark_existing && ft.is_file() {
                if let Some(rel) = rel_to_root(&self.shared.project_root, &child) {
                    handle_change(&self.shared, &rel);
                }
            }
        }
    }

    /// Close and forget the watch for a directory that errored/was removed.
    fn unwatch_dir(&mut self, dir: &Path) {
        if self.watched_dirs.remove(dir) {
            if let Some(w) = self.watcher.as_mut() {
                let _ = w.unwatch(dir);
            }
        }
    }

    /// Linux per-directory event handler (TS: `handleDirEvent`). A new
    /// sub-directory is picked up by extending the watch tree; everything else
    /// is routed through the shared change handler. If the path vanished
    /// (rapid create/delete) the stat fails and we fall through to the change
    /// handler, which no-ops on a non-source path.
    fn handle_dir_event(&mut self, full: &Path) {
        if let Ok(md) = std::fs::metadata(full) {
            if md.is_dir() {
                if !should_ignore_dir(&self.shared, full) {
                    self.watch_tree(full, /* mark_existing */ true);
                }
                return;
            }
        }
        if let Some(rel) = rel_to_root(&self.shared.project_root, full) {
            handle_change(&self.shared, &rel);
        }
    }

    fn process_event(&mut self, event: notify::Event) {
        let stopped = self.shared.state.lock().unwrap().stopped;
        if stopped {
            return;
        }
        if !is_index_relevant_event(&event.kind) {
            return;
        }
        let paths: Vec<PathBuf> = event.paths;
        for p in paths {
            if self.per_directory {
                self.handle_dir_event(&p);
            } else if let Some(rel) = rel_to_root(&self.shared.project_root, &p) {
                handle_change(&self.shared, &rel);
            }
        }
    }

    fn process_error(&mut self, err: notify::Error) {
        if self.per_directory {
            // TS: a per-directory watcher's 'error' handler unwatches that dir.
            let paths: Vec<PathBuf> = err.paths.clone();
            for p in paths {
                self.unwatch_dir(&p);
            }
        } else {
            log_warn(
                "File watcher error",
                Some(&serde_json::json!({ "error": err.to_string() })),
            );
        }
    }

    /// Flush pending changes by running sync (TS: `flush()`).
    ///
    /// pending_files is NOT cleared at the start of sync — entries are removed
    /// only after sync commits successfully, and only for entries whose
    /// last_seen_ms <= sync_started_ms. That way, a query that arrives mid-sync
    /// still sees the affected files marked stale (the DB hasn't been updated
    /// yet), and an event that lands mid-sync persists into the follow-up.
    ///
    /// On sync failure pending_files is left untouched — every edit is still
    /// unindexed, and the rescheduled sync will absorb the same set next time.
    fn flush(&mut self) {
        {
            let mut st = self.shared.state.lock().unwrap();
            // If already syncing, the post-sync check will re-trigger.
            if st.syncing || st.stopped {
                return;
            }
            st.sync_started_ms = now_ms();
            st.syncing = true;
        }

        let pending_before = self.shared.state.lock().unwrap().pending_files.len();
        let _span = linkscope::phase("watcher.flush");
        linkscope::event_fields(
            "watcher.flush.started",
            [linkscope::TraceField::count(
                "pending_files",
                pending_before as u64,
            )],
        );

        let result = (self.shared.sync_fn)();

        match &result {
            Ok(r) => {
                // Remove entries whose most recent event predates this sync —
                // those edits are now in the DB. Entries with last_seen_ms >
                // sync_started_ms arrived mid-sync; whether the in-flight sync
                // captured them depends on when sync read that file, so we keep
                // them as pending and let the follow-up sync handle them. We
                // prefer false positives ("shown stale, actually fresh" → at
                // worst one extra Read) over false negatives ("shown fresh,
                // actually stale" → misleads the agent).
                {
                    let mut st = self.shared.state.lock().unwrap();
                    let started = st.sync_started_ms;
                    st.pending_files
                        .retain(|_, info| info.last_seen_ms > started);
                }
                if let Some(cb) = &self.shared.on_sync_complete {
                    cb(*r);
                }
            }
            Err(err) => {
                if err.downcast_ref::<LockUnavailableError>().is_some() {
                    // Lock-failure no-op (another writer holds the lock).
                    // pending_files stays intact and the `finally` path below
                    // reschedules. Debug-only — a long external index would
                    // otherwise spam stderr every cycle.
                    let pending = self.shared.state.lock().unwrap().pending_files.len();
                    log_debug(
                        "Watch sync skipped: file lock unavailable",
                        Some(&serde_json::json!({ "pendingFiles": pending })),
                    );
                } else {
                    log_warn(
                        "Watch sync failed",
                        Some(&serde_json::json!({ "error": err.to_string() })),
                    );
                    if let Some(cb) = &self.shared.on_sync_error {
                        cb(err);
                    }
                }
                // Failure: leave pending_files untouched. Every edit it tracks
                // is still unindexed; the rescheduled sync sees the same set.
            }
        }

        // finally
        let mut st = self.shared.state.lock().unwrap();
        st.syncing = false;
        // If pending files remain (mid-sync events, or this sync failed),
        // schedule another pass.
        let rescheduled = !st.pending_files.is_empty() && !st.stopped;
        if rescheduled {
            st.deadline = Some(Instant::now() + Duration::from_millis(self.shared.debounce_ms));
        }
        linkscope::event_fields(
            "watcher.flush.finished",
            [
                linkscope::TraceField::count("pending_files", st.pending_files.len() as u64),
                linkscope::TraceField::text("rescheduled", rescheduled.to_string()),
                linkscope::TraceField::text("ok", result.is_ok().to_string()),
            ],
        );
    }

    /// The worker's event loop: receive watch events / seam kicks, and fire
    /// the debounced flush when the deadline elapses.
    fn run(&mut self) {
        loop {
            let timeout = {
                let st = self.shared.state.lock().unwrap();
                if st.stopped {
                    break;
                }
                st.deadline
                    .map(|d| d.saturating_duration_since(Instant::now()))
            };

            let msg = match timeout {
                Some(t) => self.rx.recv_timeout(t),
                None => self.rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };

            match msg {
                Ok(WorkerMsg::Fs(Ok(event))) => self.process_event(event),
                Ok(WorkerMsg::Fs(Err(err))) => self.process_error(err),
                Ok(WorkerMsg::Kick) => { /* recompute deadline at loop top */ }
                Ok(WorkerMsg::Stop) => break,
                Err(RecvTimeoutError::Timeout) => {
                    let due = {
                        let mut st = self.shared.state.lock().unwrap();
                        match st.deadline {
                            // The deadline may have been pushed out by an event
                            // that arrived while we were waiting.
                            Some(d) if Instant::now() >= d => {
                                st.deadline = None;
                                true
                            }
                            _ => false,
                        }
                    };
                    if due {
                        self.flush();
                    }
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        // Dropping `self.watcher` here tears down the OS watch set.
    }
}

// =============================================================================
// FileWatcher (public handle)
// =============================================================================

struct WorkerHandle {
    join: std::thread::JoinHandle<()>,
}

/// FileWatcher monitors a project directory for changes and triggers
/// debounced sync operations via a provided callback.
///
/// Design goals (same as TS):
/// - Bounded resource usage: O(1) descriptors on macOS/Windows (one recursive
///   watch), O(directories) inotify watches on Linux — never O(files), which
///   was the system-crashing fd leak on macOS (#644/#496/#555/#628).
/// - Debounced to avoid thrashing on rapid saves
/// - Filters to supported source files by extension
/// - Ignores .codegraph/ and .git/ regardless of .gitignore
/// - Tracks per-file pending state so MCP tools can flag stale results
///   without blocking on a sync (issue #403)
///
/// All methods take `&self` (interior mutability) so the wiring layer can hold
/// the watcher behind an `Arc` and drive it from any thread.
pub struct FileWatcher {
    shared: Arc<WatcherShared>,
    worker: Mutex<Option<WorkerHandle>>,
}

impl FileWatcher {
    pub fn new(project_root: impl Into<PathBuf>, sync_fn: SyncFn, options: WatchOptions) -> Self {
        let project_root: PathBuf = project_root.into();
        let project_root_str = project_root.to_string_lossy().to_string();
        FileWatcher {
            shared: Arc::new(WatcherShared {
                project_root,
                project_root_str,
                debounce_ms: options.debounce_ms.unwrap_or(DEFAULT_DEBOUNCE_MS),
                sync_fn,
                on_sync_complete: options.on_sync_complete,
                on_sync_error: options.on_sync_error,
                inert_for_tests: options.inert_for_tests,
                state: Mutex::new(WatcherState {
                    active: false,
                    inert: false,
                    stopped: false,
                    ready: false,
                    syncing: false,
                    sync_started_ms: 0,
                    deadline: None,
                    pending_files: BTreeMap::new(),
                    ignore: None,
                }),
                ready_cv: Condvar::new(),
                tx: Mutex::new(None),
            }),
            worker: Mutex::new(None),
        }
    }

    /// Start watching for file changes.
    /// Returns true if watching started successfully, false otherwise.
    pub fn start(&self) -> bool {
        let mut worker_slot = self.worker.lock().unwrap();
        {
            let mut st = self.shared.state.lock().unwrap();
            if st.active {
                return true; // Already watching
            }
            st.stopped = false;
        }

        // Some environments make filesystem watching unusable — most notably
        // WSL2 /mnt/ drives, where the underlying watch calls block long
        // enough to break MCP startup handshakes (issue #199). Skip watching
        // there; callers fall back to manual `codegraph sync` or git sync hooks.
        if let Some(reason) =
            watch_disabled_reason(&self.shared.project_root_str, &WatchProbe::default())
        {
            log_debug(
                "File watcher disabled",
                Some(&serde_json::json!({
                    "reason": reason,
                    "projectRoot": self.shared.project_root_str,
                })),
            );
            return false;
        }

        // Reuse the indexer's ignore set so the watcher and indexer agree on scope.
        let ignore = Arc::new(build_default_ignore(&self.shared.project_root));
        self.shared.state.lock().unwrap().ignore = Some(ignore);

        let (tx, rx) = unbounded::<WorkerMsg>();
        let (setup_tx, setup_rx) = bounded::<notify::Result<SetupInfo>>(1);
        let shared = Arc::clone(&self.shared);
        let worker_tx = tx.clone();

        let spawned = std::thread::Builder::new()
            .name("codegraph-file-watcher".to_string())
            // 16 MiB stack: the watcher re-extracts changed files (recursive AST
            // walkers) and walks the directory tree to install watches. Those
            // are stacker-guarded, but a roomy base stack avoids segment churn.
            .stack_size(16 * 1024 * 1024)
            .spawn(move || {
                let mut worker = Worker {
                    per_directory: !shared.inert_for_tests && !supports_recursive_watch(),
                    shared,
                    rx,
                    tx: worker_tx,
                    watcher: None,
                    watched_dirs: HashSet::new(),
                    dir_cap_warned: false,
                };
                let setup = worker.setup();
                let ok = setup.is_ok();
                let _ = setup_tx.send(setup);
                if ok {
                    worker.run();
                }
            });

        let join = match spawned {
            Ok(j) => j,
            Err(err) => {
                log_warn(
                    "Could not start file watcher",
                    Some(&serde_json::json!({ "error": err.to_string() })),
                );
                self.stop_with_slot(&mut worker_slot);
                return false;
            }
        };

        match setup_rx.recv() {
            Ok(Ok(info)) => {
                {
                    let mut st = self.shared.state.lock().unwrap();
                    // No async crawl to wait on: as soon as the watch set is
                    // installed we have a clean baseline (pending_files is only
                    // populated by post-start events). Clear defensively and
                    // flip ready.
                    st.pending_files.clear();
                    st.ready = true;
                    st.inert = self.shared.inert_for_tests;
                    st.active = info.active;
                }
                self.shared.ready_cv.notify_all();
                *self.shared.tx.lock().unwrap() = Some(tx);
                *worker_slot = Some(WorkerHandle { join });

                if is_test_runtime() {
                    live_watchers_for_tests().lock().unwrap().insert(
                        self.shared.project_root_str.clone(),
                        Arc::downgrade(&self.shared),
                    );
                }

                let mut ctx = serde_json::json!({
                    "projectRoot": self.shared.project_root_str,
                    "debounceMs": self.shared.debounce_ms,
                    "mode": info.mode,
                });
                if info.watched_dirs > 0 {
                    ctx["watchedDirs"] = serde_json::json!(info.watched_dirs);
                }
                log_debug("File watcher started", Some(&ctx));
                true
            }
            Ok(Err(err)) => {
                // Watcher setup failed (e.g., permission denied, missing directory).
                let _ = join.join();
                log_warn(
                    "Could not start file watcher",
                    Some(&serde_json::json!({ "error": err.to_string() })),
                );
                self.stop_with_slot(&mut worker_slot);
                false
            }
            Err(_) => {
                // Worker died before reporting — treat as setup failure.
                let _ = join.join();
                log_warn(
                    "Could not start file watcher",
                    Some(&serde_json::json!({ "error": "watcher thread exited during setup" })),
                );
                self.stop_with_slot(&mut worker_slot);
                false
            }
        }
    }

    /// Stop watching for file changes. Idempotent.
    pub fn stop(&self) {
        let mut worker_slot = self.worker.lock().unwrap();
        self.stop_with_slot(&mut worker_slot);
    }

    fn stop_with_slot(&self, worker_slot: &mut Option<WorkerHandle>) {
        {
            let mut st = self.shared.state.lock().unwrap();
            st.stopped = true;
            st.deadline = None;
            st.pending_files.clear();
            st.ready = false;
            st.inert = false;
            st.active = false;
            st.ignore = None;
        }
        let tx = self.shared.tx.lock().unwrap().take();
        if let Some(tx) = tx {
            let _ = tx.send(WorkerMsg::Stop);
        }
        if let Some(handle) = worker_slot.take() {
            // Don't self-join if stop() is called from a sync callback running
            // on the worker thread — the worker exits on its own (it observes
            // `stopped` / the Stop message).
            if handle.join.thread().id() != std::thread::current().id() {
                let _ = handle.join.join();
            }
        }
        if is_test_runtime() {
            live_watchers_for_tests()
                .lock()
                .unwrap()
                .remove(&self.shared.project_root_str);
        }
        log_debug("File watcher stopped", None);
    }

    /// @internal Test-only: feed a synthetic project-relative change through
    /// the same filter → pending_files → debounced-sync path a real watch
    /// event takes. Lets the watcher / staleness-banner suites stay
    /// deterministic instead of racing on OS watch-delivery latency. See
    /// [`emit_watch_event_for_tests`].
    pub fn ingest_event_for_tests(&self, rel_path: &str) {
        handle_change(&self.shared, &normalize_path(rel_path));
        kick(&self.shared);
    }

    /// @internal Test-only: feed a synthetic notify event through the worker's
    /// raw event branch. Unlike [`FileWatcher::ingest_event_for_tests`], this
    /// exercises notify event-kind filtering before reaching `handle_change`.
    pub fn ingest_notify_event_for_tests(&self, kind: EventKind, rel_path: &str) {
        let normalized = normalize_path(rel_path);
        let event = notify::Event::new(kind).add_path(self.shared.project_root.join(normalized));
        if let Some(tx) = self.shared.tx.lock().unwrap().as_ref() {
            let _ = tx.send(WorkerMsg::Fs(Ok(event)));
        }
    }

    /// Whether the watcher is currently active.
    pub fn is_active(&self) -> bool {
        let st = self.shared.state.lock().unwrap();
        st.active && !st.stopped
    }

    /// Blocks until the watch set has been installed (or returns immediately
    /// if it already has). Useful for tests that need a deterministic boundary
    /// before asserting on `pending_files`. (TS returned a Promise; `start()`
    /// here is fully synchronous, so this is effectively instant after a
    /// successful `start()` — kept for API parity.)
    pub fn wait_until_ready(&self, timeout_ms: u64) -> crate::error::Result<()> {
        let guard = self.shared.state.lock().unwrap();
        let (guard, res) = self
            .shared
            .ready_cv
            .wait_timeout_while(guard, Duration::from_millis(timeout_ms), |st| !st.ready)
            .unwrap();
        drop(guard);
        if res.timed_out() {
            Err(crate::error::CodeGraphError::other(format!(
                "FileWatcher.waitUntilReady timed out after {timeout_ms}ms"
            )))
        } else {
            Ok(())
        }
    }

    /// Snapshot of files seen by the watcher since the last successful sync.
    ///
    /// Used by MCP tool responses to mark stale results without blocking on a
    /// sync: a tool that returns a hit in `src/foo.ts` while `src/foo.ts` is in
    /// this list tells the agent "Read this file directly, the index lags."
    ///
    /// `indexing` is true when a sync is currently in flight whose start time
    /// is AFTER this file's most recent event — i.e. that sync will absorb the
    /// edit. False means the file is still inside the debounce window and no
    /// sync has started yet.
    ///
    /// Cheap: O(pending_files.len()), no I/O, no blocking work under the lock.
    pub fn get_pending_files(&self) -> Vec<PendingFile> {
        let st = self.shared.state.lock().unwrap();
        st.pending_files
            .iter()
            .map(|(path, info)| PendingFile {
                path: path.clone(),
                first_seen_ms: info.first_seen_ms,
                last_seen_ms: info.last_seen_ms,
                indexing: st.syncing && st.sync_started_ms >= info.last_seen_ms,
            })
            .collect()
    }
}

impl Drop for FileWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use notify::event::{CreateKind, DataChange, RemoveKind, RenameMode};

    use super::*;

    #[test]
    fn lock_unavailable_error_default_message_matches_ts() {
        assert_eq!(
            LockUnavailableError::new().to_string(),
            "CodeGraph file lock unavailable; another process is writing"
        );
    }

    #[test]
    fn lock_unavailable_error_downcasts_through_sync_error() {
        // Deliberately constructs via `new()` boxed into `SyncError` — the
        // exact shape `sync_fn` callers use (clippy's `Box::default()` would
        // not coerce to the `Box<dyn Error>` alias anyway).
        #[allow(clippy::box_default)]
        let err: SyncError = Box::new(LockUnavailableError::new());
        assert!(err.downcast_ref::<LockUnavailableError>().is_some());
        let other: SyncError = Box::new(std::io::Error::other("boom"));
        assert!(other.downcast_ref::<LockUnavailableError>().is_none());
    }

    #[test]
    fn is_always_ignored_covers_codegraph_and_git() {
        assert!(is_always_ignored(".codegraph"));
        assert!(is_always_ignored(".codegraph/db.sqlite"));
        assert!(is_always_ignored(".git"));
        assert!(is_always_ignored(".git/HEAD"));
        assert!(!is_always_ignored("src/index.ts"));
        assert!(!is_always_ignored(".github/workflows/ci.yml"));
    }

    #[test]
    fn rel_to_root_guards_root_and_outside_paths() {
        let root = Path::new("/proj");
        assert_eq!(rel_to_root(root, Path::new("/proj")), None);
        assert_eq!(rel_to_root(root, Path::new("/elsewhere/a.ts")), None);
        assert_eq!(
            rel_to_root(root, Path::new("/proj/src/a.ts")),
            Some("src/a.ts".to_string())
        );
    }

    #[test]
    fn index_relevant_event_ignores_non_mutating_access() {
        assert!(!is_index_relevant_event(&EventKind::Access(
            AccessKind::Any
        )));
        assert!(!is_index_relevant_event(&EventKind::Access(
            AccessKind::Read
        )));
        assert!(!is_index_relevant_event(&EventKind::Access(
            AccessKind::Open(AccessMode::Read)
        )));
        assert!(!is_index_relevant_event(&EventKind::Access(
            AccessKind::Close(AccessMode::Read)
        )));
        assert!(!is_index_relevant_event(&EventKind::Access(
            AccessKind::Open(AccessMode::Execute)
        )));
        assert!(!is_index_relevant_event(&EventKind::Access(
            AccessKind::Open(AccessMode::Other)
        )));
        assert!(!is_index_relevant_event(&EventKind::Modify(
            ModifyKind::Metadata(MetadataKind::AccessTime)
        )));
        assert!(!is_index_relevant_event(&EventKind::Other));
    }

    #[test]
    fn index_relevant_event_keeps_mutations_and_close_write() {
        assert!(is_index_relevant_event(&EventKind::Any));
        assert!(is_index_relevant_event(&EventKind::Create(
            CreateKind::File
        )));
        assert!(is_index_relevant_event(&EventKind::Remove(
            RemoveKind::File
        )));
        assert!(is_index_relevant_event(&EventKind::Modify(
            ModifyKind::Data(DataChange::Content)
        )));
        assert!(is_index_relevant_event(&EventKind::Modify(
            ModifyKind::Metadata(MetadataKind::WriteTime)
        )));
        assert!(is_index_relevant_event(&EventKind::Modify(
            ModifyKind::Name(RenameMode::Both)
        )));
        assert!(is_index_relevant_event(&EventKind::Access(
            AccessKind::Open(AccessMode::Write)
        )));
        assert!(is_index_relevant_event(&EventKind::Access(
            AccessKind::Close(AccessMode::Write)
        )));
    }

    #[test]
    fn max_dir_watches_defaults_when_env_unset_or_invalid() {
        // Note: relies on CODEGRAPH_MAX_DIR_WATCHES being unset in the test
        // environment (we don't mutate process env from unit tests).
        if std::env::var("CODEGRAPH_MAX_DIR_WATCHES").is_err() {
            assert_eq!(max_dir_watches(), DEFAULT_MAX_DIR_WATCHES);
        }
    }

    #[test]
    fn pending_file_serializes_camel_case() {
        let p = PendingFile {
            path: "src/a.ts".into(),
            first_seen_ms: 1,
            last_seen_ms: 2,
            indexing: false,
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["path"], "src/a.ts");
        assert_eq!(v["firstSeenMs"], 1);
        assert_eq!(v["lastSeenMs"], 2);
        assert_eq!(v["indexing"], false);
    }

    #[test]
    fn watch_sync_result_serializes_camel_case() {
        let r = WatchSyncResult {
            files_changed: 3,
            duration_ms: 42,
        };
        let v = serde_json::to_value(r).unwrap();
        assert_eq!(v["filesChanged"], 3);
        assert_eq!(v["durationMs"], 42);
    }
}

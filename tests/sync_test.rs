//! Sync Module Tests
//!
//! Ports of:
//! - `__tests__/watcher.test.ts` (FileWatcher: lifecycle, debounce, filtering,
//!   pending-file tracking #403, callbacks, lock-retry #449)
//! - `__tests__/watch-policy.test.ts` ("FileWatcher honors the watch policy" —
//!   the pure `watchDisabledReason` cases live as unit tests in
//!   `src/sync/watch_policy.rs`)
//! - `__tests__/worktree-detection.test.ts` (the direct-detection suite; the
//!   ToolHandler half needs the MCP port — see rust/notes/sync.md)
//!
//! **Why inert mode + the synthetic event seam**: under a parallel test runner
//! the OS watch subsystems serve many tests at once and event-delivery latency
//! is non-deterministic. So the unit-style tests construct the watcher with
//! `inert_for_tests: true` (no OS watcher installed) and drive its filter →
//! pending_files → debounce pipeline directly via
//! `ingest_event_for_tests` — deterministic, the same convergence point a real
//! event reaches. The debounce timer itself is the real worker-thread deadline
//! (the unit under test). The end-to-end tests at the bottom run the genuine
//! native (notify) watcher against real file writes, with generous timeouts.
//!
//! Env-var discipline: tests that MUTATE process env (CODEGRAPH_NO_WATCH,
//! NODE_ENV) take the ENV_LOCK write lock; every test that calls
//! `FileWatcher::start()` (which reads those vars) takes the read lock.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use codegraph::sync::{
    DEFAULT_READY_TIMEOUT_MS,
    FileWatcher,
    LockUnavailableError,
    SyncError,
    SyncFn,
    WatchOptions,
    WatchSyncResult,
    detect_worktree_index_mismatch,
    emit_watch_event_for_tests,
    git_worktree_root,
    worktree_mismatch_warning,
};
use notify::event::{AccessKind, AccessMode, DataChange, EventKind, ModifyKind};
use tempfile::TempDir;

static ENV_LOCK: RwLock<()> = RwLock::new(());

/// Helper to wait for a condition with timeout (TS: `waitFor`). Used for
/// assertions that depend on the debounce deadline firing, or on real watcher
/// event delivery in the end-to-end tests.
fn wait_for(mut condition: impl FnMut() -> bool, timeout_ms: u64) {
    let start = Instant::now();
    loop {
        if condition() {
            return;
        }
        if start.elapsed() > Duration::from_millis(timeout_ms) {
            panic!("wait_for timed out after {timeout_ms}ms");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Temp project dir with one source file so the directory isn't empty.
fn test_project() -> TempDir {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir(&src).unwrap();
    fs::write(src.join("index.ts"), "export const x = 1;").unwrap();
    dir
}

/// A `sync_fn` that counts invocations and returns `result` on every call.
fn counting_sync(result: WatchSyncResult) -> (SyncFn, Arc<AtomicUsize>) {
    let count = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&count);
    let f: SyncFn = Arc::new(move || {
        c.fetch_add(1, Ordering::SeqCst);
        Ok(result)
    });
    (f, count)
}

/// Inert by default — these tests drive events via `ingest_event_for_tests`
/// and never depend on real OS watch delivery. (TS: `newWatcher`.)
fn new_watcher(dir: &Path, sync_fn: SyncFn, mut opts: WatchOptions) -> FileWatcher {
    opts.inert_for_tests = true;
    FileWatcher::new(dir, sync_fn, opts)
}

fn opts_debounce(ms: u64) -> WatchOptions {
    WatchOptions {
        debounce_ms: Some(ms),
        ..Default::default()
    }
}

// =============================================================================
// start/stop lifecycle
// =============================================================================

#[test]
fn starts_and_stops_without_errors() {
    let _env = ENV_LOCK.read().unwrap();
    let dir = test_project();
    let (sync_fn, _count) = counting_sync(WatchSyncResult {
        files_changed: 0,
        duration_ms: 0,
    });
    let watcher = new_watcher(dir.path(), sync_fn, WatchOptions::default());

    assert!(watcher.start());
    assert!(watcher.is_active());

    watcher.stop();
    assert!(!watcher.is_active());
}

#[test]
fn is_idempotent_on_double_start() {
    let _env = ENV_LOCK.read().unwrap();
    let dir = test_project();
    let (sync_fn, _count) = counting_sync(WatchSyncResult {
        files_changed: 0,
        duration_ms: 0,
    });
    let watcher = new_watcher(dir.path(), sync_fn, WatchOptions::default());

    assert!(watcher.start());
    assert!(watcher.start()); // Should not error
    assert!(watcher.is_active());

    watcher.stop();
}

#[test]
fn is_idempotent_on_double_stop() {
    let _env = ENV_LOCK.read().unwrap();
    let dir = test_project();
    let (sync_fn, _count) = counting_sync(WatchSyncResult {
        files_changed: 0,
        duration_ms: 0,
    });
    let watcher = new_watcher(dir.path(), sync_fn, WatchOptions::default());

    watcher.start();
    watcher.stop();
    watcher.stop(); // Should not error

    assert!(!watcher.is_active());
}

// =============================================================================
// debounced sync
// =============================================================================

#[test]
fn triggers_sync_after_file_change() {
    let _env = ENV_LOCK.read().unwrap();
    let dir = test_project();
    let (sync_fn, count) = counting_sync(WatchSyncResult {
        files_changed: 1,
        duration_ms: 10,
    });
    let watcher = new_watcher(dir.path(), sync_fn, opts_debounce(200));

    watcher.start();
    watcher.wait_until_ready(DEFAULT_READY_TIMEOUT_MS).unwrap();
    watcher.ingest_event_for_tests("src/new.ts");

    // Wait for debounced sync to fire (real deadline; 200ms + epsilon).
    wait_for(|| count.load(Ordering::SeqCst) > 0, 5000);

    watcher.stop();
}

#[test]
fn debounces_rapid_changes_into_a_single_sync() {
    let _env = ENV_LOCK.read().unwrap();
    let dir = test_project();
    let (sync_fn, count) = counting_sync(WatchSyncResult {
        files_changed: 1,
        duration_ms: 10,
    });
    let watcher = new_watcher(dir.path(), sync_fn, opts_debounce(400));

    watcher.start();
    watcher.wait_until_ready(DEFAULT_READY_TIMEOUT_MS).unwrap();

    // Rapid-fire synthesized changes — each call resets the debounce deadline.
    // Spacing them tighter than the debounce window proves the debounce
    // collapses them into one sync_fn call.
    for i in 0..5 {
        watcher.ingest_event_for_tests(&format!("src/file{i}.ts"));
        std::thread::sleep(Duration::from_millis(50));
    }

    // Wait for the single debounced sync.
    wait_for(|| count.load(Ordering::SeqCst) > 0, 5000);

    // Should have been called once (debounced), not 5 times.
    assert_eq!(count.load(Ordering::SeqCst), 1);

    watcher.stop();
}

// =============================================================================
// filtering
// =============================================================================

#[test]
fn ignores_files_not_matching_source_extensions() {
    let _env = ENV_LOCK.read().unwrap();
    let dir = test_project();
    let (sync_fn, count) = counting_sync(WatchSyncResult {
        files_changed: 0,
        duration_ms: 0,
    });
    let watcher = new_watcher(dir.path(), sync_fn, opts_debounce(200));

    watcher.start();
    watcher.wait_until_ready(DEFAULT_READY_TIMEOUT_MS).unwrap();

    // A non-source-file event — FileWatcher's `is_source_file` gate must drop
    // it before scheduling sync.
    watcher.ingest_event_for_tests("src/readme.md");

    // Wait a bit longer than debounce — sync should NOT trigger.
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(count.load(Ordering::SeqCst), 0);

    watcher.stop();
}

#[test]
fn ignores_codegraph_directory_changes() {
    let _env = ENV_LOCK.read().unwrap();
    let dir = test_project();
    let (sync_fn, count) = counting_sync(WatchSyncResult {
        files_changed: 0,
        duration_ms: 0,
    });
    let watcher = new_watcher(dir.path(), sync_fn, opts_debounce(200));

    watcher.start();
    watcher.wait_until_ready(DEFAULT_READY_TIMEOUT_MS).unwrap();

    // A .codegraph event — FileWatcher's always-ignored filter must drop it
    // before scheduling sync.
    watcher.ingest_event_for_tests(".codegraph/db.sqlite");

    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(count.load(Ordering::SeqCst), 0);

    watcher.stop();
}

#[test]
fn drops_ignored_non_source_paths_but_syncs_real_source_edits() {
    let _env = ENV_LOCK.read().unwrap();
    let dir = test_project();
    let (sync_fn, count) = counting_sync(WatchSyncResult {
        files_changed: 0,
        duration_ms: 0,
    });
    let watcher = new_watcher(dir.path(), sync_fn, opts_debounce(200));

    watcher.start();
    watcher.wait_until_ready(DEFAULT_READY_TIMEOUT_MS).unwrap();

    // node_modules is in the default-ignore set (#407) → dropped by the
    // ignore matcher even without a .gitignore.
    watcher.ingest_event_for_tests("node_modules/dep/index.js");
    assert!(
        !watcher
            .get_pending_files()
            .iter()
            .any(|p| p.path.starts_with("node_modules/")),
        "ignored path must not become pending"
    );

    // A normal source file still schedules sync (positive control).
    watcher.ingest_event_for_tests("src/live.ts");
    wait_for(|| count.load(Ordering::SeqCst) > 0, 5000);

    watcher.stop();
}

#[test]
fn raw_notify_read_events_do_not_mark_source_pending() {
    let _env = ENV_LOCK.read().unwrap();
    let dir = test_project();
    let (sync_fn, count) = counting_sync(WatchSyncResult {
        files_changed: 1,
        duration_ms: 10,
    });
    let watcher = new_watcher(dir.path(), sync_fn, opts_debounce(200));

    watcher.start();
    watcher.wait_until_ready(DEFAULT_READY_TIMEOUT_MS).unwrap();

    watcher.ingest_notify_event_for_tests(
        EventKind::Access(AccessKind::Open(AccessMode::Read)),
        "src/index.ts",
    );
    std::thread::sleep(Duration::from_millis(500));
    assert!(watcher.get_pending_files().is_empty());
    assert_eq!(count.load(Ordering::SeqCst), 0);

    watcher.ingest_notify_event_for_tests(
        EventKind::Modify(ModifyKind::Data(DataChange::Content)),
        "src/index.ts",
    );
    wait_for(
        || {
            watcher
                .get_pending_files()
                .iter()
                .any(|p| p.path == "src/index.ts")
        },
        1000,
    );
    wait_for(|| count.load(Ordering::SeqCst) > 0, 5000);

    watcher.stop();
}

// =============================================================================
// pending file tracking (#403)
// =============================================================================

#[test]
fn exposes_edited_paths_via_get_pending_files_before_sync_fires() {
    let _env = ENV_LOCK.read().unwrap();
    let dir = test_project();
    // Slow debounce — pending entries are visible until the debounce fires.
    // The synthetic event is synchronous, so we can assert immediately.
    let (sync_fn, _count) = counting_sync(WatchSyncResult {
        files_changed: 1,
        duration_ms: 10,
    });
    let watcher = new_watcher(dir.path(), sync_fn, opts_debounce(2000));

    watcher.start();
    watcher.wait_until_ready(DEFAULT_READY_TIMEOUT_MS).unwrap();

    assert!(watcher.get_pending_files().is_empty());

    watcher.ingest_event_for_tests("src/pending.ts");

    let pending = watcher.get_pending_files();
    assert!(pending.iter().any(|p| p.path == "src/pending.ts"));
    let entry = pending.iter().find(|p| p.path == "src/pending.ts").unwrap();
    assert!(entry.first_seen_ms > 0);
    assert!(entry.last_seen_ms >= entry.first_seen_ms);
    // No sync running yet → indexing flag is false.
    assert!(!entry.indexing);

    watcher.stop();
}

#[test]
fn clears_an_entry_only_after_a_successful_sync_absorbing_that_edit() {
    let _env = ENV_LOCK.read().unwrap();
    let dir = test_project();
    let (sync_fn, count) = counting_sync(WatchSyncResult {
        files_changed: 1,
        duration_ms: 10,
    });
    let watcher = new_watcher(dir.path(), sync_fn, opts_debounce(200));

    watcher.start();
    watcher.wait_until_ready(DEFAULT_READY_TIMEOUT_MS).unwrap();

    watcher.ingest_event_for_tests("src/fresh.ts");

    // Watcher saw the change → pending_files has the entry IMMEDIATELY.
    assert!(
        watcher
            .get_pending_files()
            .iter()
            .any(|p| p.path == "src/fresh.ts")
    );

    // Wait through debounce + sync; the entry should drop out.
    wait_for(|| count.load(Ordering::SeqCst) > 0, 5000);
    wait_for(
        || {
            !watcher
                .get_pending_files()
                .iter()
                .any(|p| p.path == "src/fresh.ts")
        },
        5000,
    );

    assert!(watcher.get_pending_files().is_empty());
    watcher.stop();
}

#[test]
fn keeps_entries_unchanged_when_sync_fails_rescheduled_work_sees_same_set() {
    let _env = ENV_LOCK.read().unwrap();
    let dir = test_project();

    // First sync fails, retry succeeds (TS: mockRejectedValueOnce).
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in_sync = Arc::clone(&calls);
    let sync_fn: SyncFn = Arc::new(move || {
        if calls_in_sync.fetch_add(1, Ordering::SeqCst) == 0 {
            Err::<WatchSyncResult, SyncError>(Box::new(std::io::Error::other("boom")))
        } else {
            Ok(WatchSyncResult {
                files_changed: 1,
                duration_ms: 10,
            })
        }
    });

    let errors = Arc::new(AtomicUsize::new(0));
    let errors_cb = Arc::clone(&errors);
    let watcher = new_watcher(
        dir.path(),
        sync_fn,
        WatchOptions {
            debounce_ms: Some(250),
            on_sync_error: Some(Arc::new(move |_err: &SyncError| {
                errors_cb.fetch_add(1, Ordering::SeqCst);
            })),
            ..Default::default()
        },
    );
    watcher.start();
    watcher.wait_until_ready(DEFAULT_READY_TIMEOUT_MS).unwrap();

    watcher.ingest_event_for_tests("src/will-fail.ts");

    // Wait for the sync to fail.
    wait_for(|| errors.load(Ordering::SeqCst) > 0, 5000);

    // The file is STILL in pending_files — failure didn't drop it.
    assert!(
        watcher
            .get_pending_files()
            .iter()
            .any(|p| p.path == "src/will-fail.ts")
    );

    // Retry resolves automatically; entry clears.
    wait_for(
        || {
            !watcher
                .get_pending_files()
                .iter()
                .any(|p| p.path == "src/will-fail.ts")
        },
        5000,
    );

    watcher.stop();
}

#[test]
fn retains_pending_files_and_retries_on_lock_unavailable_error_449() {
    let _env = ENV_LOCK.read().unwrap();
    let dir = test_project();

    // CodeGraph.watch() converts the cross-process lock-failure no-op into
    // LockUnavailableError so the watcher's retry path picks it up instead of
    // falsely clearing pending_files. This test exercises the contract directly.
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in_sync = Arc::clone(&calls);
    let sync_fn: SyncFn = Arc::new(move || {
        if calls_in_sync.fetch_add(1, Ordering::SeqCst) == 0 {
            Err::<WatchSyncResult, SyncError>(Box::new(LockUnavailableError::new()))
        } else {
            Ok(WatchSyncResult {
                files_changed: 1,
                duration_ms: 10,
            })
        }
    });

    let completions: Arc<Mutex<Vec<WatchSyncResult>>> = Arc::new(Mutex::new(Vec::new()));
    let completions_cb = Arc::clone(&completions);
    let errors = Arc::new(AtomicUsize::new(0));
    let errors_cb = Arc::clone(&errors);
    let watcher = new_watcher(
        dir.path(),
        sync_fn,
        WatchOptions {
            debounce_ms: Some(250),
            on_sync_complete: Some(Arc::new(move |r| {
                completions_cb.lock().unwrap().push(r);
            })),
            on_sync_error: Some(Arc::new(move |_err: &SyncError| {
                errors_cb.fetch_add(1, Ordering::SeqCst);
            })),
            ..Default::default()
        },
    );
    watcher.start();
    watcher.wait_until_ready(DEFAULT_READY_TIMEOUT_MS).unwrap();

    watcher.ingest_event_for_tests("src/locked.ts");

    wait_for(|| calls.load(Ordering::SeqCst) >= 1, 5000);
    assert!(
        watcher
            .get_pending_files()
            .iter()
            .any(|p| p.path == "src/locked.ts")
    );
    // A held-lock no-op is not a sync failure — on_sync_error stays quiet
    // so a long-running external indexer doesn't spam stderr every cycle.
    assert_eq!(errors.load(Ordering::SeqCst), 0);
    assert!(completions.lock().unwrap().is_empty());

    wait_for(|| calls.load(Ordering::SeqCst) >= 2, 5000);
    wait_for(
        || {
            !watcher
                .get_pending_files()
                .iter()
                .any(|p| p.path == "src/locked.ts")
        },
        5000,
    );

    let completed = completions.lock().unwrap().clone();
    assert_eq!(
        completed,
        vec![WatchSyncResult {
            files_changed: 1,
            duration_ms: 10
        }]
    );
    assert_eq!(errors.load(Ordering::SeqCst), 0);

    watcher.stop();
}

// =============================================================================
// callbacks
// =============================================================================

#[test]
fn calls_on_sync_complete_after_successful_sync() {
    let _env = ENV_LOCK.read().unwrap();
    let dir = test_project();
    let (sync_fn, _count) = counting_sync(WatchSyncResult {
        files_changed: 2,
        duration_ms: 50,
    });

    let completions: Arc<Mutex<Vec<WatchSyncResult>>> = Arc::new(Mutex::new(Vec::new()));
    let completions_cb = Arc::clone(&completions);
    let watcher = new_watcher(
        dir.path(),
        sync_fn,
        WatchOptions {
            debounce_ms: Some(200),
            on_sync_complete: Some(Arc::new(move |r| {
                completions_cb.lock().unwrap().push(r);
            })),
            ..Default::default()
        },
    );

    watcher.start();
    watcher.wait_until_ready(DEFAULT_READY_TIMEOUT_MS).unwrap();
    watcher.ingest_event_for_tests("src/test.ts");

    wait_for(|| !completions.lock().unwrap().is_empty(), 5000);
    assert_eq!(
        completions.lock().unwrap()[0],
        WatchSyncResult {
            files_changed: 2,
            duration_ms: 50
        }
    );

    watcher.stop();
}

#[test]
fn calls_on_sync_error_when_sync_fails() {
    let _env = ENV_LOCK.read().unwrap();
    let dir = test_project();
    let sync_fn: SyncFn = Arc::new(|| {
        Err::<WatchSyncResult, SyncError>(Box::new(std::io::Error::other("sync failed")))
    });

    let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let errors_cb = Arc::clone(&errors);
    let watcher = new_watcher(
        dir.path(),
        sync_fn,
        WatchOptions {
            debounce_ms: Some(200),
            on_sync_error: Some(Arc::new(move |err: &SyncError| {
                errors_cb.lock().unwrap().push(err.to_string());
            })),
            ..Default::default()
        },
    );

    watcher.start();
    watcher.wait_until_ready(DEFAULT_READY_TIMEOUT_MS).unwrap();
    watcher.ingest_event_for_tests("src/test.ts");

    wait_for(|| !errors.lock().unwrap().is_empty(), 5000);
    assert_eq!(errors.lock().unwrap()[0], "sync failed");

    // Always-failing sync reschedules forever; stop() must cleanly end it.
    watcher.stop();
}

// =============================================================================
// watch policy (port of "FileWatcher honors the watch policy")
// =============================================================================

#[test]
fn does_not_start_when_codegraph_no_watch_is_set() {
    let _env = ENV_LOCK.write().unwrap(); // mutates process env
    let dir = test_project();
    std::env::set_var("CODEGRAPH_NO_WATCH", "1");

    let (sync_fn, _count) = counting_sync(WatchSyncResult {
        files_changed: 0,
        duration_ms: 0,
    });
    // NOT inert: the policy check runs before any watcher would be installed.
    let watcher = FileWatcher::new(dir.path(), sync_fn, WatchOptions::default());

    let started = watcher.start();
    std::env::remove_var("CODEGRAPH_NO_WATCH");

    assert!(!started);
    assert!(!watcher.is_active());
}

// =============================================================================
// emit_watch_event_for_tests registry (TS: __emitWatchEventForTests)
// =============================================================================

#[test]
fn registry_routes_synthetic_events_only_under_a_test_runtime() {
    let _env = ENV_LOCK.write().unwrap(); // mutates process env
    let dir = test_project();
    let root = dir.path().to_string_lossy().to_string();
    std::env::set_var("NODE_ENV", "test");

    let (sync_fn, _count) = counting_sync(WatchSyncResult {
        files_changed: 1,
        duration_ms: 10,
    });
    let watcher = new_watcher(dir.path(), sync_fn, opts_debounce(2000));
    watcher.start();

    // Live watcher registered → events route through the real pipeline.
    assert!(emit_watch_event_for_tests(&root, "src/seam.ts"));
    assert!(
        watcher
            .get_pending_files()
            .iter()
            .any(|p| p.path == "src/seam.ts")
    );

    // Unknown root → false.
    assert!(!emit_watch_event_for_tests("/no/such/project", "src/x.ts"));

    watcher.stop();
    std::env::remove_var("NODE_ENV");

    // Deregistered on stop → false.
    assert!(!emit_watch_event_for_tests(&root, "src/seam.ts"));
}

// =============================================================================
// end-to-end: the genuine native watcher (notify) against real file writes
// =============================================================================

#[test]
fn auto_syncs_when_files_change_while_watching_real_watcher_end_to_end() {
    let _env = ENV_LOCK.read().unwrap();
    let dir = test_project();
    let (sync_fn, count) = counting_sync(WatchSyncResult {
        files_changed: 1,
        duration_ms: 10,
    });
    // NOT inert — the one test that exercises real OS event delivery
    // (per-directory inotify on Linux, recursive FSEvents/RDCW elsewhere).
    let watcher = FileWatcher::new(dir.path(), sync_fn, opts_debounce(300));

    assert!(watcher.start());
    // Let the watch set settle before writing, so the event isn't missed.
    std::thread::sleep(Duration::from_millis(100));

    // Real fs write — no synthetic event. The live watcher must catch it.
    fs::write(
        dir.path().join("src").join("added.ts"),
        "export function added() { return 42; }",
    )
    .unwrap();

    // Real OS event delivery + debounce — generous timeout.
    wait_for(|| count.load(Ordering::SeqCst) > 0, 15_000);

    watcher.stop();
}

#[test]
fn real_watcher_picks_up_files_in_a_directory_created_after_start() {
    let _env = ENV_LOCK.read().unwrap();
    let dir = test_project();
    let (sync_fn, count) = counting_sync(WatchSyncResult {
        files_changed: 1,
        duration_ms: 10,
    });
    let watcher = FileWatcher::new(dir.path(), sync_fn, opts_debounce(300));

    assert!(watcher.start());
    std::thread::sleep(Duration::from_millis(100));

    // New directory + immediate write — on the Linux per-directory path this
    // exercises the dynamic watch_tree(mark_existing=true) race closure.
    let newdir = dir.path().join("feature");
    fs::create_dir(&newdir).unwrap();
    fs::write(newdir.join("late.ts"), "export const late = true;").unwrap();

    wait_for(|| count.load(Ordering::SeqCst) > 0, 15_000);

    watcher.stop();
}

#[test]
fn real_watcher_does_not_descend_into_default_ignored_trees() {
    let _env = ENV_LOCK.read().unwrap();
    let dir = test_project();
    fs::create_dir_all(dir.path().join("node_modules").join("dep")).unwrap();

    let (sync_fn, count) = counting_sync(WatchSyncResult {
        files_changed: 0,
        duration_ms: 0,
    });
    let watcher = FileWatcher::new(dir.path(), sync_fn, opts_debounce(200));
    assert!(watcher.start());
    std::thread::sleep(Duration::from_millis(100));

    // Churn inside an ignored tree must never schedule a sync (#276 / #407).
    fs::write(
        dir.path().join("node_modules").join("dep").join("index.js"),
        "module.exports = 1;",
    )
    .unwrap();

    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(count.load(Ordering::SeqCst), 0);

    watcher.stop();
}

// =============================================================================
// worktree detection (port of __tests__/worktree-detection.test.ts, issue #155)
// =============================================================================

mod worktree {
    use std::path::PathBuf;
    use std::process::Command;

    use super::*;

    fn git(cwd: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("git should be runnable");
        assert!(status.success(), "git {args:?} failed in {}", cwd.display());
    }

    /// realpath so macOS /var → /private/var symlinking doesn't break equality.
    fn real(p: &Path) -> PathBuf {
        fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
    }

    /// Main checkout (owns the index) + a linked worktree nested INSIDE it,
    /// mirroring tools that place worktrees under paths like
    /// `.claude/worktrees/<name>/`.
    fn setup() -> (TempDir, PathBuf) {
        let main_repo = TempDir::new().unwrap();
        let root = main_repo.path();
        git(root, &["init", "-q"]);
        git(root, &["config", "user.email", "test@example.com"]);
        git(root, &["config", "user.name", "Test"]);
        git(root, &["config", "commit.gpgsign", "false"]);
        fs::write(root.join("README.md"), "# main\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", "init"]);

        let worktree = root.join("wt");
        git(
            root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feature",
                worktree.to_str().unwrap(),
            ],
        );
        (main_repo, worktree)
    }

    #[test]
    fn flags_a_worktree_borrowing_the_main_checkout_index() {
        let (main_repo, worktree) = setup();
        let m = detect_worktree_index_mismatch(&worktree, main_repo.path())
            .expect("mismatch should be detected");
        assert_eq!(m.worktree_root, real(&worktree));
        assert_eq!(m.index_root, real(main_repo.path()));
    }

    #[test]
    fn returns_none_when_the_index_lives_in_the_same_working_tree() {
        let (main_repo, worktree) = setup();
        assert!(detect_worktree_index_mismatch(main_repo.path(), main_repo.path()).is_none());
        assert!(detect_worktree_index_mismatch(&worktree, &worktree).is_none());
    }

    #[test]
    fn returns_none_for_a_subdirectory_of_the_same_working_tree() {
        let (main_repo, _worktree) = setup();
        let sub = main_repo.path().join("src");
        fs::create_dir(&sub).unwrap();
        assert!(detect_worktree_index_mismatch(&sub, main_repo.path()).is_none());
    }

    #[test]
    fn returns_none_when_start_path_is_not_in_a_git_repo() {
        let (main_repo, _worktree) = setup();
        let non_git = TempDir::new().unwrap();
        assert!(detect_worktree_index_mismatch(non_git.path(), main_repo.path()).is_none());
    }

    #[test]
    fn returns_none_when_the_index_root_is_a_plain_non_worktree_directory() {
        // start_path is a real worktree, but the index sits in an unrelated
        // non-git dir — that's "index in an ancestor", not "borrowed another
        // worktree".
        let (_main_repo, worktree) = setup();
        let non_git = TempDir::new().unwrap();
        assert!(detect_worktree_index_mismatch(&worktree, non_git.path()).is_none());
    }

    #[test]
    fn git_worktree_root_reports_each_tree_distinctly() {
        let (main_repo, worktree) = setup();
        let non_git = TempDir::new().unwrap();
        assert_eq!(git_worktree_root(&worktree), Some(real(&worktree)));
        assert_eq!(
            git_worktree_root(main_repo.path()),
            Some(real(main_repo.path()))
        );
        assert_eq!(git_worktree_root(non_git.path()), None);
    }

    #[test]
    fn warning_names_both_trees_and_the_fix() {
        let (main_repo, worktree) = setup();
        let m = detect_worktree_index_mismatch(&worktree, main_repo.path()).unwrap();
        let msg = worktree_mismatch_warning(&m);
        assert!(msg.contains(&real(&worktree).display().to_string()));
        assert!(msg.contains(&real(main_repo.path()).display().to_string()));
        assert!(msg.contains("codegraph init"));
    }
}

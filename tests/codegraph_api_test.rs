//! Public-API (`CodeGraph` facade) integration tests.
//!
//! Ports the suites that drive the TS `CodeGraph` class end-to-end and were
//! deferred to the public-API wave:
//! - `__tests__/sync.test.ts` (all of it — Sync Functionality + Git-based sync)
//! - `__tests__/concurrent-locking.test.ts` (#238 — the DB-pragma/WAL cases;
//!   the ToolHandler describe belongs to the MCP wave) plus facade-level
//!   FileLock contention semantics
//! - `__tests__/security.test.ts` "Path Traversal Prevention" (getCode cases)
//! - `__tests__/foundation.test.ts` facade-level cases deferred by notes/ui.md
//!   (Opening Projects, Database stats, Close/Destroy, Graph Query Methods)
//! - `__tests__/watcher.test.ts` "CodeGraph integration" describe
//! - the deferred end-to-end cases from `__tests__/extraction.test.ts` (IDA C
//!   callers/callees after indexAll) and `__tests__/object-literal-methods.test.ts`
//! - an end-to-end smoke test (stable node/edge counts across re-index,
//!   callers/callees resolve)
//!
//! Real files, real SQLite, real git — no mocks (TS suite parity).

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use codegraph::{CodeGraph, IndexOptions, NodeKind, SearchOptions, Severity, WatchOptions};
use tempfile::TempDir;

// =============================================================================
// Helpers
// =============================================================================

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

/// `beforeEach` of the TS sync suites: a `src/index.ts` with `hello()`,
/// then `initSync` + `indexAll`.
fn setup_indexed(root: &Path) -> CodeGraph {
    write(
        &root.join("src/index.ts"),
        "export function hello() { return 'world'; }",
    );
    let cg = CodeGraph::init_sync(root).unwrap();
    cg.index_all(&IndexOptions::default()).unwrap();
    cg
}

fn git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
        ])
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("git should be runnable");
    assert!(status.success(), "git {args:?} failed in {}", cwd.display());
}

fn git_stdout(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git should be runnable");
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// `beforeEach` of the TS "Git-based sync" describe: git repo with an initial
/// commit of `src/index.ts`, then `initSync` + `indexAll`.
fn setup_git_indexed(root: &Path) -> CodeGraph {
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    write(
        &root.join("src/index.ts"),
        "export function hello() { return 'world'; }",
    );
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "initial"]);
    let cg = CodeGraph::init_sync(root).unwrap();
    cg.index_all(&IndexOptions::default()).unwrap();
    cg
}

fn search_count(cg: &CodeGraph, query: &str) -> usize {
    cg.search_nodes(query, None).unwrap().len()
}

/// Poll until `predicate` is true, or panic after `timeout`.
fn wait_for(mut predicate: impl FnMut() -> bool, timeout: Duration, what: &str) {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if predicate() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for: {what}");
}

#[cfg(target_os = "linux")]
fn current_thread_names() -> Vec<String> {
    let Ok(entries) = fs::read_dir("/proc/self/task") else {
        return Vec::new();
    };

    entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| fs::read_to_string(entry.path().join("comm")).ok())
        .map(|name| name.trim().to_string())
        .collect()
}

// =============================================================================
// sync.test.ts — Sync Functionality (hash-based, no git)
// =============================================================================

mod sync_functionality {
    use super::*;

    #[test]
    fn get_changed_files_detects_added_files() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path());

        write(
            &dir.path().join("src/new.ts"),
            "export function newFunc() { return 42; }",
        );

        let changes = cg.get_changed_files().unwrap();
        assert!(changes.added.contains(&"src/new.ts".to_string()));
        assert!(changes.modified.is_empty());
        assert!(changes.removed.is_empty());
    }

    #[test]
    fn get_changed_files_detects_modified_files() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path());

        write(
            &dir.path().join("src/index.ts"),
            "export function hello() { return 'modified'; }",
        );

        let changes = cg.get_changed_files().unwrap();
        assert!(changes.added.is_empty());
        assert!(changes.modified.contains(&"src/index.ts".to_string()));
        assert!(changes.removed.is_empty());
    }

    #[test]
    fn get_changed_files_detects_removed_files() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path());

        fs::remove_file(dir.path().join("src/index.ts")).unwrap();

        let changes = cg.get_changed_files().unwrap();
        assert!(changes.added.is_empty());
        assert!(changes.modified.is_empty());
        assert!(changes.removed.contains(&"src/index.ts".to_string()));
    }

    #[test]
    fn sync_reindexes_added_files() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path());

        write(
            &dir.path().join("src/new.ts"),
            "export function newFunc() { return 42; }",
        );

        let result = cg.sync(&IndexOptions::default()).unwrap();
        assert_eq!(result.files_added, 1);
        assert_eq!(result.files_modified, 0);
        assert_eq!(result.files_removed, 0);

        // Verify new function is in the graph
        assert!(search_count(&cg, "newFunc") > 0);
    }

    #[test]
    fn sync_reindexes_modified_files() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path());

        write(
            &dir.path().join("src/index.ts"),
            "export function goodbye() { return 'farewell'; }",
        );

        let result = cg.sync(&IndexOptions::default()).unwrap();
        assert_eq!(result.files_modified, 1);

        // Verify new function is in the graph, old one is gone
        assert!(search_count(&cg, "goodbye") > 0);
        assert_eq!(search_count(&cg, "hello"), 0);
    }

    #[test]
    fn sync_removes_nodes_from_deleted_files() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path());

        fs::remove_file(dir.path().join("src/index.ts")).unwrap();

        let result = cg.sync(&IndexOptions::default()).unwrap();
        assert_eq!(result.files_removed, 1);
        assert_eq!(search_count(&cg, "hello"), 0);
    }

    #[test]
    fn sync_reports_no_changes_when_nothing_changed() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path());

        let result = cg.sync(&IndexOptions::default()).unwrap();
        assert_eq!(result.files_added, 0);
        assert_eq!(result.files_modified, 0);
        assert_eq!(result.files_removed, 0);
        // A real no-op sync still scans: filesChecked > 0 (this is the
        // property the watcher's lock-failure zero-shape detection rides on).
        assert!(result.files_checked > 0);
    }

    #[test]
    fn index_all_reconciles_nodes_from_deleted_files() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path());

        fs::remove_file(dir.path().join("src/index.ts")).unwrap();

        let result = cg.index_all(&IndexOptions::default()).unwrap();
        assert!(result.success);
        assert_eq!(search_count(&cg, "hello"), 0);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn index_all_does_not_leave_persistent_parse_worker_threads() {
        let dir = TempDir::new().unwrap();
        for i in 0..96 {
            write(
                &dir.path().join(format!("src/file_{i}.ts")),
                &format!("export function f{i}() {{ return {i}; }}"),
            );
        }

        let cg = CodeGraph::init_sync(dir.path()).unwrap();
        let result = cg.index_all(&IndexOptions::default()).unwrap();
        assert!(result.success);

        let parse_threads: Vec<String> = current_thread_names()
            .into_iter()
            .filter(|name| name.starts_with("cg-parse-"))
            .collect();
        assert!(
            parse_threads.is_empty(),
            "index_all left persistent parse worker threads behind: {parse_threads:?}"
        );
    }

    #[test]
    fn index_all_keeps_unresolved_refs_repairable_by_a_later_full_index() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path());

        write(
            &dir.path().join("src/index.ts"),
            "export function caller() { return missingLater(); }",
        );
        cg.index_all(&IndexOptions::default()).unwrap();

        // A second full index used to wipe unresolved_refs even though the
        // target still did not exist, making a later target addition invisible.
        cg.index_all(&IndexOptions::default()).unwrap();

        write(
            &dir.path().join("src/target.ts"),
            "export function missingLater() { return 42; }",
        );
        cg.index_all(&IndexOptions::default()).unwrap();

        let target = cg
            .search_nodes("missingLater", None)
            .unwrap()
            .into_iter()
            .map(|r| r.node)
            .find(|n| n.kind == NodeKind::Function)
            .expect("target function should be indexed");
        let callers = cg.get_callers(&target.id, None).unwrap();
        assert!(
            callers.iter().any(|r| r.node.name == "caller"),
            "caller should resolve to the late-added target"
        );
    }

    #[test]
    fn index_files_resolves_refs_and_repairs_late_targets() {
        let dir = TempDir::new().unwrap();
        write(
            &dir.path().join("src/caller.ts"),
            "export function caller() { return missingLater(); }",
        );
        let cg = CodeGraph::init_sync(dir.path()).unwrap();

        let first = cg.index_files(&["src/caller.ts".to_string()]).unwrap();
        assert!(first.success);
        assert_eq!(first.files_indexed, 1);

        write(
            &dir.path().join("src/target.ts"),
            "export function missingLater() { return 42; }",
        );
        let second = cg.index_files(&["src/target.ts".to_string()]).unwrap();
        assert!(second.success);
        assert_eq!(second.files_indexed, 1);

        let target = cg
            .search_nodes("missingLater", None)
            .unwrap()
            .into_iter()
            .map(|r| r.node)
            .find(|n| n.kind == NodeKind::Function)
            .expect("target function should be indexed");
        let callers = cg.get_callers(&target.id, None).unwrap();
        assert!(
            callers.iter().any(|r| r.node.name == "caller"),
            "index_files should resolve existing refs after a late target is indexed"
        );
    }

    #[test]
    fn sync_repairs_callers_when_removed_target_reappears() {
        let dir = TempDir::new().unwrap();
        write(
            &dir.path().join("src/caller.ts"),
            "import { missingLater } from './target';\nexport function caller() { return missingLater(); }",
        );
        write(
            &dir.path().join("src/target.ts"),
            "export function missingLater() { return 42; }",
        );
        let cg = CodeGraph::init_sync(dir.path()).unwrap();
        cg.index_all(&IndexOptions::default()).unwrap();

        let target = cg
            .search_nodes("missingLater", None)
            .unwrap()
            .into_iter()
            .map(|r| r.node)
            .find(|n| n.kind == NodeKind::Function)
            .expect("target function should be indexed");
        assert!(
            cg.get_callers(&target.id, None)
                .unwrap()
                .iter()
                .any(|r| r.node.name == "caller"),
            "initial full index should resolve caller"
        );

        fs::remove_file(dir.path().join("src/target.ts")).unwrap();
        let removed = cg.sync(&IndexOptions::default()).unwrap();
        assert_eq!(removed.files_removed, 1);
        assert!(
            !cg.search_nodes("missingLater", None)
                .unwrap()
                .into_iter()
                .any(|r| r.node.kind == NodeKind::Function),
            "deleted target function should not remain indexed"
        );

        write(
            &dir.path().join("src/target.ts"),
            "export function missingLater() { return 42; }",
        );
        let added = cg.sync(&IndexOptions::default()).unwrap();
        assert_eq!(added.files_added, 1);

        let restored_target = cg
            .search_nodes("missingLater", None)
            .unwrap()
            .into_iter()
            .map(|r| r.node)
            .find(|n| n.kind == NodeKind::Function)
            .expect("target function should be re-indexed");
        let callers = cg.get_callers(&restored_target.id, None).unwrap();
        assert!(
            callers.iter().any(|r| r.node.name == "caller"),
            "sync should restore caller edge when an unchanged caller's target reappears"
        );
    }

    #[test]
    fn search_pagination_pages_after_final_scoring_and_filtering() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path());

        for i in 0..130 {
            write(
                &dir.path().join(format!("src/vendor/alpha-{i}.ts")),
                &format!("export function alphaVendor{i}() {{ return {i}; }}"),
            );
        }
        for i in 0..8 {
            write(
                &dir.path().join(format!("src/focused/alpha-{i}.ts")),
                &format!("export function alphaFocused{i}() {{ return {i}; }}"),
            );
        }
        cg.index_all(&IndexOptions::default()).unwrap();

        let all = cg
            .search_nodes(
                "alpha path:focused",
                Some(&SearchOptions {
                    limit: Some(8),
                    ..Default::default()
                }),
            )
            .unwrap();
        let first = cg
            .search_nodes(
                "alpha path:focused",
                Some(&SearchOptions {
                    limit: Some(3),
                    offset: Some(0),
                    ..Default::default()
                }),
            )
            .unwrap();
        let second = cg
            .search_nodes(
                "alpha path:focused",
                Some(&SearchOptions {
                    limit: Some(3),
                    offset: Some(3),
                    ..Default::default()
                }),
            )
            .unwrap();

        assert_eq!(all.len(), 8);
        let ids =
            |v: &[codegraph::SearchResult]| v.iter().map(|r| r.node.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&first), ids(&all[0..3]));
        assert_eq!(ids(&second), ids(&all[3..6]));
        let mut combined = ids(&first);
        combined.extend(ids(&second));
        combined.sort();
        combined.dedup();
        assert_eq!(combined.len(), 6);
    }
}

// =============================================================================
// sync.test.ts — Git-based sync
// =============================================================================

mod git_based_sync {
    use super::*;

    #[test]
    fn detects_modified_files_via_git() {
        let dir = TempDir::new().unwrap();
        let cg = setup_git_indexed(dir.path());

        write(
            &dir.path().join("src/index.ts"),
            "export function hello() { return 'modified'; }",
        );

        let result = cg.sync(&IndexOptions::default()).unwrap();
        assert_eq!(result.files_modified, 1);
        assert!(
            result
                .changed_file_paths
                .as_deref()
                .unwrap_or(&[])
                .contains(&"src/index.ts".to_string())
        );
    }

    #[test]
    fn detects_new_untracked_files_via_git() {
        let dir = TempDir::new().unwrap();
        let cg = setup_git_indexed(dir.path());

        write(
            &dir.path().join("src/new.ts"),
            "export function newFunc() { return 42; }",
        );

        let result = cg.sync(&IndexOptions::default()).unwrap();
        assert_eq!(result.files_added, 1);
        assert!(
            result
                .changed_file_paths
                .as_deref()
                .unwrap_or(&[])
                .contains(&"src/new.ts".to_string())
        );

        // Verify the function was indexed
        assert!(search_count(&cg, "newFunc") > 0);
    }

    #[test]
    fn stops_reporting_untracked_files_once_indexed_issue_206() {
        let dir = TempDir::new().unwrap();
        let cg = setup_git_indexed(dir.path());

        // Untracked files stay `??` in git status even after codegraph indexes
        // them. Change detection must compare them against the DB by hash, not
        // report every untracked file as "added" on every sync/status.
        write(
            &dir.path().join("src/new.ts"),
            "export function newFunc() { return 42; }",
        );

        // First sync indexes the untracked file.
        let first = cg.sync(&IndexOptions::default()).unwrap();
        assert_eq!(first.files_added, 1);

        // The file is still untracked in git, but now lives in the DB.
        assert!(search_count(&cg, "newFunc") > 0);

        // status must not keep flagging it as a pending addition...
        let changes = cg.get_changed_files().unwrap();
        assert!(!changes.added.contains(&"src/new.ts".to_string()));
        assert!(!changes.modified.contains(&"src/new.ts".to_string()));

        // ...and a second sync must be a no-op for it.
        let second = cg.sync(&IndexOptions::default()).unwrap();
        assert_eq!(second.files_added, 0);
        assert_eq!(second.files_modified, 0);
    }

    #[test]
    fn reindexes_an_untracked_file_when_its_contents_change() {
        let dir = TempDir::new().unwrap();
        let cg = setup_git_indexed(dir.path());

        let file_path = dir.path().join("src/new.ts");
        write(&file_path, "export function newFunc() { return 42; }");
        cg.sync(&IndexOptions::default()).unwrap();

        // Modify the still-untracked file.
        write(&file_path, "export function renamedFunc() { return 7; }");

        let changes = cg.get_changed_files().unwrap();
        assert!(changes.modified.contains(&"src/new.ts".to_string()));

        let result = cg.sync(&IndexOptions::default()).unwrap();
        assert_eq!(result.files_modified, 1);
        assert!(search_count(&cg, "renamedFunc") > 0);
        assert_eq!(search_count(&cg, "newFunc"), 0);
    }

    #[test]
    fn detects_deleted_files_via_git() {
        let dir = TempDir::new().unwrap();
        let cg = setup_git_indexed(dir.path());

        fs::remove_file(dir.path().join("src/index.ts")).unwrap();

        let result = cg.sync(&IndexOptions::default()).unwrap();
        assert_eq!(result.files_removed, 1);

        // Verify function is gone
        assert_eq!(search_count(&cg, "hello"), 0);
    }

    #[test]
    fn indexes_a_tracked_file_that_grows_large_instead_of_dropping_it() {
        let dir = TempDir::new().unwrap();
        let cg = setup_git_indexed(dir.path());

        assert!(search_count(&cg, "hello") > 0);

        // There is no size cap: a file that grows past 1 MiB is re-indexed
        // (not purged), so its new symbol appears and the old one is gone.
        let mut oversized = String::from("export function replacement() { return 1; }\n");
        oversized.push_str(&"x".repeat(2 * 1024 * 1024));
        write(&dir.path().join("src/index.ts"), &oversized);

        let result = cg.sync(&IndexOptions::default()).unwrap();
        assert_eq!(result.files_modified, 1);
        assert_eq!(search_count(&cg, "hello"), 0);
        assert!(search_count(&cg, "replacement") > 0);
    }

    #[test]
    fn resolves_existing_unresolved_refs_when_a_later_sync_adds_the_target_symbol() {
        let dir = TempDir::new().unwrap();
        let cg = setup_git_indexed(dir.path());

        write(
            &dir.path().join("src/index.ts"),
            "export function caller() { return missingTarget(); }",
        );
        cg.sync(&IndexOptions::default()).unwrap();

        write(
            &dir.path().join("src/target.ts"),
            "export function missingTarget() { return 42; }",
        );

        let result = cg.sync(&IndexOptions::default()).unwrap();
        assert_eq!(result.files_added, 1);

        let target = cg
            .search_nodes("missingTarget", None)
            .unwrap()
            .into_iter()
            .map(|r| r.node)
            .find(|n| n.kind == NodeKind::Function)
            .expect("target function should be indexed");
        let callers = cg.get_callers(&target.id, None).unwrap();
        assert!(callers.iter().any(|r| r.node.name == "caller"));
    }

    #[test]
    fn skips_files_with_unsupported_extensions() {
        let dir = TempDir::new().unwrap();
        let cg = setup_git_indexed(dir.path());

        // A .txt file has no supported grammar, so sync must not index it.
        write(&dir.path().join("src/notes.txt"), "just some notes");

        let result = cg.sync(&IndexOptions::default()).unwrap();
        assert_eq!(result.files_added, 0);
        assert_eq!(result.files_modified, 0);
    }

    #[test]
    fn reports_no_changes_on_clean_working_tree() {
        let dir = TempDir::new().unwrap();
        let cg = setup_git_indexed(dir.path());

        let result = cg.sync(&IndexOptions::default()).unwrap();
        assert_eq!(result.files_added, 0);
        assert_eq!(result.files_modified, 0);
        assert_eq!(result.files_removed, 0);
        // TS: `expect(result.changedFilePaths).toBeUndefined()`
        assert!(result.changed_file_paths.is_none());
    }

    #[test]
    fn reports_files_changed_on_disk_even_when_git_status_is_clean() {
        let dir = TempDir::new().unwrap();
        let cg = setup_git_indexed(dir.path());

        write(
            &dir.path().join("src/index.ts"),
            "export function hello() { return 'from second commit'; }",
        );
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "second"]);
        cg.sync(&IndexOptions::default()).unwrap();

        // Move the working tree to a different committed version. `git status`
        // is clean afterward, but CodeGraph's DB still reflects the first commit.
        git(dir.path(), &["checkout", "-q", "HEAD~1"]);
        let status = git_stdout(dir.path(), &["status", "--porcelain"]);
        assert!(!status.contains("src/index.ts"));

        let changes = cg.get_changed_files().unwrap();
        assert!(changes.modified.contains(&"src/index.ts".to_string()));
    }
}

// =============================================================================
// concurrent-locking.test.ts — issue #238 (DB pragmas + WAL concurrency)
// =============================================================================

mod concurrent_locking {
    use codegraph::DatabaseConnection;

    use super::*;

    #[test]
    fn uses_a_bounded_busy_timeout_not_the_old_2_minute_hang() {
        let dir = TempDir::new().unwrap();
        let conn = DatabaseConnection::initialize(dir.path().join("codegraph.db")).unwrap();
        let db = conn.get_db().unwrap();
        let ms: i64 = db
            .conn()
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert!(ms > 0);
        assert!(ms <= 30_000); // far below the old 120000
    }

    #[test]
    fn runs_in_wal_mode_and_get_journal_mode_surfaces_it() {
        let dir = TempDir::new().unwrap();
        let conn = DatabaseConnection::initialize(dir.path().join("codegraph.db")).unwrap();
        assert_eq!(conn.get_journal_mode().unwrap(), "wal");
    }

    #[test]
    fn a_read_on_a_2nd_connection_succeeds_while_a_writer_holds_the_lock() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("codegraph.db");
        let writer = DatabaseConnection::initialize(&db_path).unwrap();
        // The property only holds under WAL; skip if the filesystem couldn't
        // enable it (TS skips the same way).
        if writer.get_journal_mode().unwrap() != "wal" {
            return;
        }
        let reader = DatabaseConnection::open(&db_path).unwrap();
        let writer_db = writer.get_db().unwrap();
        writer_db.conn().execute_batch("BEGIN EXCLUSIVE").unwrap(); // hard write lock, held open

        let t0 = Instant::now();
        let count: i64 = reader
            .get_db()
            .unwrap()
            .conn()
            .query_row("SELECT COUNT(*) AS c FROM nodes", [], |r| r.get(0))
            .unwrap();
        let waited = t0.elapsed();

        writer_db.conn().execute_batch("COMMIT").unwrap();

        assert_eq!(count, 0);
        assert!(waited < Duration::from_millis(1000)); // proceeds immediately, no busy wait
    }

    /// Facade-level FileLock contention: a lock file held by a LIVE process
    /// (our own PID stands in for "another process") makes indexAll return the
    /// exact TS lock-failure result and sync return the zero-shape (#449) —
    /// without erroring, and recoverable once the lock clears.
    #[test]
    fn index_all_and_sync_surface_lock_contention_without_erroring() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path());

        let lock_path = dir.path().join(".codegraph").join("codegraph.lock");
        fs::write(&lock_path, format!("{}", std::process::id())).unwrap();

        // indexAll → the TS lock-failure shape
        let result = cg.index_all(&IndexOptions::default()).unwrap();
        assert!(!result.success);
        assert_eq!(result.files_indexed, 0);
        assert_eq!(result.duration_ms, 0);
        assert_eq!(
            result.errors[0].message,
            "Could not acquire file lock - another process may be indexing"
        );
        assert_eq!(result.errors[0].severity, Severity::Error);

        // sync → the exact zero-shape the watcher detects (#449)
        let sync_result = cg.sync(&IndexOptions::default()).unwrap();
        assert_eq!(sync_result.files_checked, 0);
        assert_eq!(sync_result.duration_ms, 0);

        // The foreign lock must not be deleted by our failed attempts.
        assert!(lock_path.exists());

        // Once the other "process" releases, operations succeed again.
        fs::remove_file(&lock_path).unwrap();
        let result = cg.index_all(&IndexOptions::default()).unwrap();
        assert!(result.success);
        let sync_result = cg.sync(&IndexOptions::default()).unwrap();
        assert!(sync_result.files_checked > 0);
    }

    /// A stale lock from a dead process is taken over (FileLock semantics
    /// surfaced through the facade).
    #[test]
    fn index_all_takes_over_a_stale_lock_from_a_dead_process() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path());

        let lock_path = dir.path().join(".codegraph").join("codegraph.lock");
        fs::write(&lock_path, "99999999").unwrap(); // dead PID

        let result = cg.index_all(&IndexOptions::default()).unwrap();
        assert!(result.success);
        // Our run released the lock afterwards.
        assert!(!lock_path.exists());
    }
}

// =============================================================================
// security.test.ts — Path Traversal Prevention (CodeGraph.getCode)
// =============================================================================

mod path_traversal_prevention {
    use super::*;

    fn setup(root: &Path) -> CodeGraph {
        write(
            &root.join("src/hello.ts"),
            "export function hello(): string { return \"hi\"; }\n",
        );
        let cg = CodeGraph::init_sync(root).unwrap();
        cg.index_all(&IndexOptions::default()).unwrap();
        cg
    }

    #[test]
    fn reads_code_for_valid_nodes_within_project() {
        let dir = TempDir::new().unwrap();
        let cg = setup(dir.path());

        let nodes = cg.get_nodes_by_kind(NodeKind::Function).unwrap();
        let hello = nodes
            .iter()
            .find(|n| n.name == "hello")
            .expect("hello should be indexed");

        let code = cg.get_code(&hello.id).unwrap();
        assert!(code.expect("code should be readable").contains("hello"));
    }

    #[test]
    fn returns_none_for_non_existent_node() {
        let dir = TempDir::new().unwrap();
        let cg = setup(dir.path());

        assert!(cg.get_code("does-not-exist").unwrap().is_none());
    }
}

// =============================================================================
// foundation.test.ts — facade-level cases deferred to the public-API wave
// =============================================================================

mod foundation_facade {
    use super::*;

    #[test]
    fn open_sync_errors_on_uninitialized_project() {
        let dir = TempDir::new().unwrap();
        let err = CodeGraph::open_sync(dir.path()).unwrap_err().to_string();
        assert!(
            err.to_lowercase().contains("not initialized"),
            "error should match /not initialized/i, got: {err}"
        );
    }

    #[test]
    fn init_sync_errors_when_already_initialized() {
        let dir = TempDir::new().unwrap();
        let cg = CodeGraph::init_sync(dir.path()).unwrap();
        cg.close();
        let err = CodeGraph::init_sync(dir.path()).unwrap_err().to_string();
        assert!(
            err.to_lowercase().contains("already initialized"),
            "error should match /already initialized/i, got: {err}"
        );
    }

    #[test]
    fn open_sync_returns_a_working_instance_with_project_root() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path());
        cg.close();

        assert!(CodeGraph::is_initialized(dir.path()));
        let reopened = CodeGraph::open_sync(dir.path()).unwrap();
        // path.resolve parity: the resolved root locates the same directory
        assert!(reopened.get_project_root().join(".codegraph").is_dir());
        assert!(search_count(&reopened, "hello") > 0);
    }

    #[test]
    fn get_stats_optimize_and_clear() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path());

        let stats = cg.get_stats().unwrap();
        assert!(stats.node_count > 0);
        assert!(stats.edge_count > 0);
        assert_eq!(stats.file_count, 1);
        assert!(stats.db_size_bytes > 0);

        cg.optimize().unwrap();

        cg.clear().unwrap();
        let cleared = cg.get_stats().unwrap();
        assert_eq!(cleared.node_count, 0);
        assert_eq!(cleared.edge_count, 0);
        assert_eq!(cleared.file_count, 0);
    }

    #[test]
    fn backend_and_journal_mode_surface_through_the_facade() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path());
        assert_eq!(cg.get_backend().as_str(), "native");
        assert_eq!(cg.get_journal_mode().unwrap(), "wal");
        assert!(cg.get_last_indexed_at().unwrap().is_some());
    }

    #[test]
    #[allow(deprecated)]
    fn destroy_alias_closes_but_keeps_codegraph_dir() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path());
        cg.destroy();
        assert!(dir.path().join(".codegraph").is_dir());
    }

    #[test]
    fn uninitialize_removes_the_codegraph_dir() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path());
        cg.uninitialize().unwrap();
        assert!(!dir.path().join(".codegraph").exists());
        assert!(!CodeGraph::is_initialized(dir.path()));
    }

    #[test]
    fn graph_query_methods_handle_unknown_nodes() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path());

        // getContext on a missing node → Err "Node not found: <id>"
        let err = cg.get_context("nonexistent").unwrap_err().to_string();
        assert!(err.contains("Node not found"), "got: {err}");

        // Traversals/usages on unknown ids → empty results (TS parity)
        assert!(cg.traverse("nonexistent", None).unwrap().nodes.is_empty());
        assert!(
            cg.get_call_graph("nonexistent", None)
                .unwrap()
                .nodes
                .is_empty()
        );
        assert!(
            cg.get_type_hierarchy("nonexistent")
                .unwrap()
                .nodes
                .is_empty()
        );
        assert!(cg.find_usages("nonexistent").unwrap().is_empty());
    }

    #[test]
    fn is_indexing_is_false_outside_and_true_inside_a_progress_callback() {
        let dir = TempDir::new().unwrap();
        write(
            &dir.path().join("src/index.ts"),
            "export function hello() { return 'world'; }",
        );
        let cg = CodeGraph::init_sync(dir.path()).unwrap();
        assert!(!cg.is_indexing());

        let observed = std::cell::Cell::new(false);
        let cg_ref = &cg;
        let on_progress = |_p: &codegraph::IndexProgress| {
            if cg_ref.is_indexing() {
                observed.set(true);
            }
        };
        cg.index_all(&IndexOptions {
            on_progress: Some(&on_progress),
            ..Default::default()
        })
        .unwrap();

        assert!(
            observed.get(),
            "is_indexing() should be true during indexAll"
        );
        assert!(!cg.is_indexing());
    }
}

// =============================================================================
// watcher.test.ts — "CodeGraph integration" describe
// =============================================================================

mod watcher_integration {
    use super::*;

    #[test]
    fn watch_and_unwatch_via_codegraph_api() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path());

        assert!(!cg.is_watching());

        let started = cg.watch(WatchOptions {
            debounce_ms: Some(200),
            inert_for_tests: true,
            ..Default::default()
        });
        assert!(started);
        assert!(cg.is_watching());
        assert!(cg.get_pending_files().is_empty());

        cg.unwatch();
        assert!(!cg.is_watching());
    }

    #[test]
    fn stops_watching_on_close() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path());

        cg.watch(WatchOptions {
            debounce_ms: Some(200),
            inert_for_tests: true,
            ..Default::default()
        });
        assert!(cg.is_watching());

        cg.close();
        assert!(!cg.is_watching());
    }

    #[test]
    fn auto_syncs_when_files_change_while_watching_real_watcher_end_to_end() {
        // The one test that exercises the genuine native watcher: a real file
        // write must propagate through OS events → debounce → sync into the
        // graph. The sync runs on the watcher's worker thread via a fresh
        // short-lived CodeGraph instance (this one is !Send); WAL makes the
        // write visible to this connection.
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path());

        let initial_nodes = cg.get_stats().unwrap().node_count;

        let started = cg.watch(WatchOptions {
            debounce_ms: Some(300),
            ..Default::default()
        });
        if !started {
            // Watch policy can disable watching in constrained environments
            // (e.g. CODEGRAPH_NO_WATCH, WSL) — nothing to assert then.
            return;
        }
        // Let the watcher install before writing, so the event isn't missed.
        cg.wait_until_watcher_ready(None).unwrap();

        // Real fs write — no synthetic event. The live watcher must catch it.
        write(
            &dir.path().join("src/added.ts"),
            "export function added() { return 42; }",
        );

        // Wait for auto-sync to pick it up (real OS event delivery + debounce).
        wait_for(
            || cg.get_stats().unwrap().node_count > initial_nodes,
            Duration::from_secs(8),
            "auto-sync to index the new file",
        );

        // The new function should be in the graph.
        assert!(search_count(&cg, "added") > 0);

        cg.unwatch();
    }
}

// =============================================================================
// Deferred end-to-end cases from other waves + smoke test
// =============================================================================

mod end_to_end {
    use codegraph::EdgeKind;

    use super::*;

    /// extraction.test.ts — "should resolve IDA sub callers and callees after
    /// indexAll" (deferred by notes/extraction-orchestrator.md).
    #[test]
    fn resolves_ida_sub_callers_and_callees_after_index_all() {
        let dir = TempDir::new().unwrap();
        write(
            &dir.path().join("sub_1000.c"),
            "__int64 __fastcall sub_1000(__int64 a1)\n{\n  return sub_2000(a1);\n}\n",
        );
        write(
            &dir.path().join("sub_2000.c"),
            "__int64 __fastcall sub_2000(__int64 a1)\n{\n  return a1 + 1;\n}\n",
        );

        let cg = CodeGraph::init(dir.path(), &codegraph::InitOptions::default()).unwrap();

        let caller = cg
            .get_nodes_in_file("sub_1000.c")
            .unwrap()
            .into_iter()
            .find(|n| n.kind == NodeKind::Function && n.name == "sub_1000")
            .expect("sub_1000 should be indexed");
        let callee = cg
            .get_nodes_in_file("sub_2000.c")
            .unwrap()
            .into_iter()
            .find(|n| n.kind == NodeKind::Function && n.name == "sub_2000")
            .expect("sub_2000 should be indexed");

        let callees = cg.get_callees(&caller.id, None).unwrap();
        assert!(
            callees
                .iter()
                .any(|r| r.node.id == callee.id && r.edge.kind == EdgeKind::Calls)
        );

        let callers = cg.get_callers(&callee.id, None).unwrap();
        assert!(
            callers
                .iter()
                .any(|r| r.node.id == caller.id && r.edge.kind == EdgeKind::Calls)
        );

        cg.close();
    }

    /// object-literal-methods.test.ts — "resolves callers of store actions
    /// across files (destructured + chained getState())" (deferred by
    /// notes/resolution-stitch.md).
    #[test]
    fn resolves_callers_of_store_actions_across_files() {
        let dir = TempDir::new().unwrap();
        write(
            &dir.path().join("package.json"),
            "{\"name\":\"t\",\"dependencies\":{\"zustand\":\"^4\"}}\n",
        );
        write(
            &dir.path().join("store.ts"),
            concat!(
                "import { create } from 'zustand'\n",
                "interface S { fetchUser(): Promise<void>; reset(): void }\n",
                "export const useStore = create<S>((set, get) => ({\n",
                "  fetchUser: async () => { get().reset() },\n",
                "  reset: () => set({}),\n",
                "}))\n",
            ),
        );
        write(
            &dir.path().join("caller.ts"),
            concat!(
                "import { useStore } from './store'\n",
                "export async function loginFlow() {\n",
                "  const { fetchUser } = useStore.getState()\n",
                "  await fetchUser()\n",
                "}\n",
                "export function hardReset() {\n",
                "  useStore.getState().reset()\n",
                "}\n",
            ),
        );

        let cg = CodeGraph::init_sync(dir.path()).unwrap();
        cg.index_all(&IndexOptions::default()).unwrap();

        let fns = cg.get_nodes_by_kind(NodeKind::Function).unwrap();
        let fetch_user = fns
            .iter()
            .find(|n| n.name == "fetchUser" && n.file_path.ends_with("store.ts"))
            .expect("fetchUser should be indexed");
        let reset = fns
            .iter()
            .find(|n| n.name == "reset" && n.file_path.ends_with("store.ts"))
            .expect("reset should be indexed");

        // Destructured-then-bare call: loginFlow -> fetchUser
        let fetch_user_callers: Vec<String> = cg
            .get_callers(&fetch_user.id, None)
            .unwrap()
            .into_iter()
            .map(|r| r.node.name)
            .collect();
        assert!(fetch_user_callers.contains(&"loginFlow".to_string()));

        // Chained getState() call: hardReset -> reset, AND in-store sibling:
        // fetchUser -> reset
        let reset_callers: Vec<String> = cg
            .get_callers(&reset.id, None)
            .unwrap()
            .into_iter()
            .map(|r| r.node.name)
            .collect();
        assert!(reset_callers.contains(&"hardReset".to_string()));
        assert!(reset_callers.contains(&"fetchUser".to_string()));

        cg.close();
    }

    /// Smoke test: index a small fixture project end-to-end; node/edge counts
    /// are stable across a re-index, and callers/callees resolve through
    /// import-based resolution.
    #[test]
    fn smoke_index_fixture_counts_stable_and_call_edges_resolve() {
        let dir = TempDir::new().unwrap();
        write(
            &dir.path().join("src/a.ts"),
            "export function helper(): number { return 1; }\n",
        );
        write(
            &dir.path().join("src/b.ts"),
            concat!(
                "import { helper } from './a';\n",
                "export function main(): number { return helper(); }\n",
            ),
        );

        let cg = CodeGraph::init_sync(dir.path()).unwrap();
        let first = cg.index_all(&IndexOptions::default()).unwrap();
        assert!(first.success);
        assert_eq!(first.files_indexed, 2);
        assert!(first.nodes_created > 0);
        assert!(first.edges_created > 0);

        let stats_first = cg.get_stats().unwrap();
        assert!(stats_first.node_count > 0);
        assert!(stats_first.edge_count > 0);
        assert_eq!(stats_first.file_count, 2);

        // Re-index: no node/edge explosion, counts stable.
        let second = cg.index_all(&IndexOptions::default()).unwrap();
        assert!(second.success);
        let stats_second = cg.get_stats().unwrap();
        assert_eq!(stats_second.node_count, stats_first.node_count);
        assert_eq!(stats_second.edge_count, stats_first.edge_count);

        // callers/callees resolve across the import.
        let helper = cg
            .search_nodes("helper", None)
            .unwrap()
            .into_iter()
            .map(|r| r.node)
            .find(|n| n.kind == NodeKind::Function && n.name == "helper")
            .expect("helper should be indexed");
        let main = cg
            .search_nodes("main", None)
            .unwrap()
            .into_iter()
            .map(|r| r.node)
            .find(|n| n.kind == NodeKind::Function && n.name == "main")
            .expect("main should be indexed");

        let callers = cg.get_callers(&helper.id, None).unwrap();
        assert!(
            callers.iter().any(|r| r.node.id == main.id),
            "main should be a caller of helper"
        );
        let callees = cg.get_callees(&main.id, None).unwrap();
        assert!(
            callees.iter().any(|r| r.node.id == helper.id),
            "helper should be a callee of main"
        );

        // The graph-shaped read APIs work end-to-end on the fixture.
        let impact = cg.get_impact_radius(&helper.id, None).unwrap();
        assert!(impact.nodes.contains_key(&main.id));
        let imports = cg.get_nodes_by_kind(NodeKind::Import).unwrap();
        assert!(imports.iter().any(|n| n.name == "./a"));
        let path = cg.find_path(&main.id, &helper.id, None).unwrap();
        assert!(path.is_some(), "a call path main -> helper should exist");

        cg.close();
    }
}

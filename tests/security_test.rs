//! Security Tests
//!
//! Port of the utils/directory-targeting cases of `__tests__/security.test.ts`:
//! - FileLock (cross-process locking)
//! - Path traversal prevention
//! - validateProjectPath — sensitive directory blocking
//! - Symlink resistance (unix-gated, like the TS `it.runIf` platform gates)
//! - Atomic writes
//!
//! The CodeGraph/MCP/extraction/db-targeting cases (Path Traversal Prevention
//! via `getCode`, MCP Input Validation, `isSourceFile`, JSON.parse error
//! boundaries, Symlink Cycle Detection in `scanDirectory`) are deferred to
//! their module waves — see `rust/notes/ui.md`.

use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use codegraph::directory::{
    create_directory,
    get_codegraph_dir,
    is_initialized,
    list_directory_contents,
    remove_directory,
};
use codegraph::utils::{
    FileLock,
    is_path_within_root,
    is_path_within_root_real,
    validate_path_within_root,
    validate_project_path,
};

// ============================================================
// describe('FileLock')
// ============================================================

// "should acquire and release a lock"
#[test]
fn file_lock_acquires_and_releases() {
    let tmp = tempfile::tempdir().unwrap();
    let lock_path = tmp.path().join("test.lock");

    let mut lock = FileLock::new(&lock_path);
    lock.acquire().unwrap();

    assert!(lock_path.exists());
    let content = fs::read_to_string(&lock_path).unwrap();
    assert_eq!(content.trim().parse::<u32>().unwrap(), std::process::id());

    lock.release();
    assert!(!lock_path.exists());
}

// "should prevent double acquisition within same process"
#[test]
fn file_lock_prevents_double_acquisition_within_same_process() {
    let tmp = tempfile::tempdir().unwrap();
    let lock_path = tmp.path().join("test.lock");

    let mut lock1 = FileLock::new(&lock_path);
    let mut lock2 = FileLock::new(&lock_path);

    lock1.acquire().unwrap();

    // Second lock should fail because our PID is alive
    let err = lock2.acquire().unwrap_err();
    assert!(
        err.to_string().contains("locked by another process"),
        "expected /locked by another process/, got: {err}"
    );

    lock1.release();
}

// "should detect and remove stale locks from dead processes"
#[test]
fn file_lock_detects_and_removes_stale_locks_from_dead_processes() {
    let tmp = tempfile::tempdir().unwrap();
    let lock_path = tmp.path().join("test.lock");

    // Write a lock file with a PID that doesn't exist.
    // PID 99999999 is extremely unlikely to be a real process.
    fs::write(&lock_path, "99999999").unwrap();

    let mut lock = FileLock::new(&lock_path);
    // Should succeed because the PID is dead
    lock.acquire().unwrap();

    lock.release();
}

// "should execute function with withLock"
#[test]
fn file_lock_executes_function_with_with_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let lock_path = tmp.path().join("test.lock");

    let mut lock = FileLock::new(&lock_path);
    let result = lock
        .with_lock(|| {
            assert!(lock_path.exists());
            42
        })
        .unwrap();

    assert_eq!(result, 42);
    assert!(!lock_path.exists());
}

// "should release lock even if function throws" — in Rust the panic
// unwinds through with_lock and FileLock's Drop releases the lock.
#[test]
fn file_lock_releases_lock_even_if_function_panics() {
    let tmp = tempfile::tempdir().unwrap();
    let lock_path = tmp.path().join("test.lock");

    let result = catch_unwind(AssertUnwindSafe(|| {
        let mut lock = FileLock::new(&lock_path);
        let _ = lock.with_lock(|| -> i32 {
            panic!("test error");
        });
    }));

    assert!(result.is_err());
    assert!(!lock_path.exists());
}

// "release should be idempotent"
#[test]
fn file_lock_release_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let lock_path = tmp.path().join("test.lock");

    let mut lock = FileLock::new(&lock_path);
    lock.acquire().unwrap();
    lock.release();
    // Second release should not throw
    lock.release();
}

// NOTE: "should execute async function with withLockAsync" and
// "should release lock even if async function throws" are dropped —
// there is no async runtime in the Rust port (PORTING.md rule 5);
// the sync with_lock covers the same contract.

// ============================================================
// describe('Path Traversal Prevention') — ported at the utils level.
// The TS block exercises this through CodeGraph.getCode (deferred to the
// public-API wave); validate_path_within_root is the mechanism under test.
// ============================================================

#[test]
fn validate_path_within_root_allows_paths_inside_project() {
    let tmp = tempfile::tempdir().unwrap();
    let resolved = validate_path_within_root(tmp.path(), "src/hello.ts");
    assert!(resolved.is_some());
    assert!(resolved.unwrap().starts_with(tmp.path()));
}

#[test]
fn validate_path_within_root_blocks_traversal_outside_project() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(validate_path_within_root(tmp.path(), "../../etc/passwd").is_none());
    assert!(validate_path_within_root(tmp.path(), "a/../../../../etc/passwd").is_none());
}

#[test]
fn is_path_within_root_matches_logical_containment() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(is_path_within_root("src/index.ts", tmp.path()));
    assert!(!is_path_within_root("../escape.ts", tmp.path()));
}

// ============================================================
// describe('validateProjectPath — sensitive directory blocking')
// ============================================================

// POSIX-only: on Windows '/etc' resolves to C:\etc (non-existent), not a
// sensitive dir — the Windows case is covered by the windows-gated test below.
// "blocks POSIX system directories (exact match)"
#[cfg(unix)]
#[test]
fn validate_project_path_blocks_posix_system_directories() {
    let root = validate_project_path(Path::new("/")).expect("/ must be blocked");
    assert!(root.to_lowercase().contains("sensitive system directory"));

    let etc = validate_project_path(Path::new("/etc")).expect("/etc must be blocked");
    assert!(etc.to_lowercase().contains("sensitive system directory"));
}

// "allows a normal, existing directory"
#[test]
fn validate_project_path_allows_a_normal_existing_directory() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(validate_project_path(dir.path()), None);
}

// SENSITIVE_PATHS stores the Windows entries lowercase and validate_project_path
// matches via the lowercased resolved path, so 'C:\Windows' and 'c:\windows'
// are both blocked. Path resolution is platform-specific, so Windows-only.
// "blocks Windows system directories regardless of case"
#[cfg(windows)]
#[test]
fn validate_project_path_blocks_windows_system_directories_regardless_of_case() {
    for p in ["C:\\Windows", "c:\\windows", "C:\\WINDOWS\\System32"] {
        let msg =
            validate_project_path(Path::new(p)).unwrap_or_else(|| panic!("{p} must be blocked"));
        assert!(msg.to_lowercase().contains("sensitive system directory"));
    }
}

// ============================================================
// Symlink resistance (unix-gated — symlink creation needs privileges on
// Windows, mirroring the TS suite's try/skip around fs.symlinkSync)
// ============================================================

// A path that *looks* inside the root but symlinks out of it must be
// rejected by the realpath-aware check.
#[cfg(unix)]
#[test]
fn is_path_within_root_real_catches_symlink_escape() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "secret").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();

    // The logical check passes (the path looks like it's inside root)…
    assert!(is_path_within_root("link/secret.txt", root.path()));
    // …but the realpath-aware check rejects the escape.
    assert!(!is_path_within_root_real("link/secret.txt", root.path()));

    // A real file inside root passes both.
    fs::write(root.path().join("ok.txt"), "ok").unwrap();
    assert!(is_path_within_root_real("ok.txt", root.path()));
}

// `remove_directory` must never follow a symlinked `.codegraph` for a
// recursive delete — it removes only the link itself.
#[cfg(unix)]
#[test]
fn remove_directory_does_not_follow_symlinked_codegraph_dir() {
    let project = tempfile::tempdir().unwrap();
    let victim = tempfile::tempdir().unwrap();
    fs::write(victim.path().join("important.txt"), "do not delete").unwrap();

    std::os::unix::fs::symlink(victim.path(), get_codegraph_dir(project.path())).unwrap();

    remove_directory(project.path()).unwrap();

    // The symlink itself is gone…
    assert!(fs::symlink_metadata(get_codegraph_dir(project.path())).is_err());
    // …but the target directory and its contents survive.
    assert!(victim.path().join("important.txt").exists());
}

#[cfg(unix)]
#[test]
fn is_initialized_rejects_symlinked_codegraph_dir() {
    let project = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    fs::write(external.path().join("codegraph.db"), "").unwrap();

    std::os::unix::fs::symlink(external.path(), get_codegraph_dir(project.path())).unwrap();

    assert!(!is_initialized(project.path()));
}

// `list_directory_contents` skips symlinks so listings never leak (or
// follow) paths outside `.codegraph/`.
#[cfg(unix)]
#[test]
fn list_directory_contents_skips_symlinks() {
    let project = tempfile::tempdir().unwrap();
    create_directory(project.path()).unwrap();
    let dir = get_codegraph_dir(project.path());
    fs::write(dir.join("real.txt"), "x").unwrap();

    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("outside.txt"), "y").unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("outside.txt"),
        dir.join("sneaky-file.txt"),
    )
    .unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.join("sneaky-dir")).unwrap();

    let files = list_directory_contents(project.path());
    assert!(files.contains(&"real.txt".to_string()));
    assert!(!files.iter().any(|f| f.contains("sneaky")));
    assert!(!files.iter().any(|f| f.contains("outside.txt")));
}

// ============================================================
// describe('Atomic Writes')
// ============================================================

// "should not leave temp files on success" — simulates what
// atomicWriteFileSync does (write `<file>.tmp.<pid>`, then rename).
#[test]
fn atomic_writes_do_not_leave_temp_files_on_success() {
    let tmp = tempfile::tempdir().unwrap();
    let config_dir = tmp.path().join(".claude");
    fs::create_dir_all(&config_dir).unwrap();

    let test_file = config_dir.join("test.json");
    let tmp_path = PathBuf::from(format!(
        "{}.tmp.{}",
        test_file.to_string_lossy(),
        std::process::id()
    ));
    fs::write(&tmp_path, "{\"test\": true}").unwrap();
    fs::rename(&tmp_path, &test_file).unwrap();

    assert!(test_file.exists());
    assert!(!tmp_path.exists());

    let content: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&test_file).unwrap()).unwrap();
    assert_eq!(content["test"], true);
}

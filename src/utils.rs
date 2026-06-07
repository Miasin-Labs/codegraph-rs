//! CodeGraph Utilities
//!
//! Ported from `src/utils.ts`. Concurrency helpers that exist only to paper
//! over Node's single-threaded event loop (Mutex-as-class, processInBatches,
//! debounce/throttle, MemoryMonitor) are intentionally NOT ported — Rust code
//! uses `std::sync` primitives, rayon, and the watcher's own debounce instead.

use std::fs;
use std::path::{Path, PathBuf};

/// Check whether a process is alive (mirrors `isProcessAlive`).
///
/// On Linux additionally treats zombie processes as dead, since `kill(pid, 0)`
/// succeeds for zombies (matters for daemon liveness checks in containers).
#[cfg(unix)]
pub fn is_process_alive(pid: u32) -> bool {
    let alive = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if alive != 0 {
        // EPERM means the process exists but we can't signal it
        return std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) {
            if let Some(close) = stat.rfind(')') {
                let state = stat[close + 1..].trim_start().chars().next();
                if state == Some('Z') {
                    return false;
                }
            }
        }
    }
    true
}

#[cfg(windows)]
pub fn is_process_alive(pid: u32) -> bool {
    // Conservative check via OpenProcess would need windows-sys; use tasklist-free
    // approach: kill with signal 0 has no direct equivalent. Treat as alive only
    // if the process can be opened.
    use std::process::Command;
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

// ============================================================
// SECURITY UTILITIES
// ============================================================

/// Sensitive system directories that should never be used as project roots.
/// Checked on all platforms; non-applicable paths are harmlessly skipped.
pub const SENSITIVE_PATHS: [&str; 18] = [
    "/",
    "/etc",
    "/usr",
    "/bin",
    "/sbin",
    "/var",
    "/tmp",
    "/dev",
    "/proc",
    "/sys",
    "/root",
    "/boot",
    "/lib",
    "/lib64",
    "/opt",
    "c:\\",
    "c:\\windows",
    "c:\\windows\\system32",
];

/// Resolve a path like Node's `path.resolve` — join onto base and normalize
/// `.`/`..` components lexically (without hitting the filesystem).
pub fn lexical_resolve(base: &Path, rel: &str) -> PathBuf {
    let joined = if Path::new(rel).is_absolute() {
        PathBuf::from(rel)
    } else {
        base.join(rel)
    };
    let mut out = PathBuf::new();
    for comp in joined.components() {
        use std::path::Component::*;
        match comp {
            CurDir => {}
            ParentDir => {
                out.pop();
            }
            c => out.push(c.as_os_str()),
        }
    }
    out
}

/// Validate that a resolved file path stays within the project root.
/// Prevents path traversal attacks (e.g. node.filePath = "../../etc/passwd").
///
/// Returns the resolved absolute path, or `None` if it escapes the root.
pub fn validate_path_within_root(project_root: &Path, file_path: &str) -> Option<PathBuf> {
    let normalized_root = lexical_resolve(Path::new(""), &project_root.to_string_lossy());
    let resolved = lexical_resolve(&normalized_root, file_path);

    if resolved == normalized_root || resolved.starts_with(&normalized_root) {
        Some(resolved)
    } else {
        None
    }
}

/// Validate an existing filesystem path using both lexical and realpath checks.
///
/// This is stricter than [`validate_path_within_root`]: symlinks are resolved
/// and the final target must still be under the canonical project root.
pub fn validate_existing_path_within_root_real(
    project_root: &Path,
    file_path: &str,
) -> Option<PathBuf> {
    let resolved = validate_path_within_root(project_root, file_path)?;
    let real_path = fs::canonicalize(&resolved).ok()?;
    let real_root = fs::canonicalize(project_root).ok()?;

    if real_path == real_root || real_path.starts_with(&real_root) {
        Some(resolved)
    } else {
        None
    }
}

/// Validate that a path is a safe project root directory.
///
/// Rejects sensitive system directories and ensures the path is a real,
/// existing directory. Used at MCP and API entry points to prevent
/// arbitrary directory access.
///
/// Returns an error message if invalid, or `None` if valid.
pub fn validate_project_path(dir_path: &Path) -> Option<String> {
    let resolved = lexical_resolve(
        &std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
        &dir_path.to_string_lossy(),
    );
    let resolved_str = resolved.to_string_lossy().to_string();
    let lower = resolved_str.to_lowercase();

    // Block sensitive system directories
    if SENSITIVE_PATHS.contains(&resolved_str.as_str()) || SENSITIVE_PATHS.contains(&lower.as_str())
    {
        return Some(format!(
            "Refusing to operate on sensitive system directory: {resolved_str}"
        ));
    }

    // Also block common sensitive home subdirectories
    if let Some(home) = dirs::home_dir() {
        for dir in [".ssh", ".gnupg", ".aws", ".config"] {
            let sensitive = home.join(dir);
            if resolved == sensitive || resolved.starts_with(&sensitive) {
                return Some(format!(
                    "Refusing to operate on sensitive directory: {resolved_str}"
                ));
            }
        }
    }

    // Verify it's a real directory
    match fs::metadata(&resolved) {
        Ok(meta) if meta.is_dir() => None,
        Ok(_) => Some(format!("Path is not a directory: {resolved_str}")),
        Err(_) => Some(format!(
            "Path does not exist or is not accessible: {resolved_str}"
        )),
    }
}

/// Check if a file path resolves to a location within the given root directory.
pub fn is_path_within_root(file_path: &str, root_dir: &Path) -> bool {
    let resolved_root = lexical_resolve(
        &std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
        &root_dir.to_string_lossy(),
    );
    let resolved_path = lexical_resolve(&resolved_root, file_path);
    resolved_path == resolved_root || resolved_path.starts_with(&resolved_root)
}

/// Like `is_path_within_root` but also resolves symlinks via `fs::canonicalize`.
///
/// This catches symlink escapes where the logical path appears to be within
/// root but the real path on disk points elsewhere. Falls back to logical
/// path checking if realpath resolution fails (e.g. broken symlink).
pub fn is_path_within_root_real(file_path: &str, root_dir: &Path) -> bool {
    if !is_path_within_root(file_path, root_dir) {
        return false;
    }
    let joined = lexical_resolve(root_dir, file_path);
    match (fs::canonicalize(&joined), fs::canonicalize(root_dir)) {
        (Ok(real_path), Ok(real_root)) => {
            real_path == real_root || real_path.starts_with(&real_root)
        }
        // If realpath fails (broken symlink, permissions), fall back to logical check
        _ => true,
    }
}

/// Safely parse JSON with a fallback value.
/// Prevents crashes from corrupted database metadata.
pub fn safe_json_parse<T: serde::de::DeserializeOwned>(value: &str, fallback: T) -> T {
    serde_json::from_str(value).unwrap_or(fallback)
}

/// Clamp a numeric value to a range.
/// Used to enforce sane limits on MCP tool inputs.
pub fn clamp<T: PartialOrd>(value: T, min: T, max: T) -> T {
    if value < min {
        min
    } else if value > max {
        max
    } else {
        value
    }
}

/// Normalize a file path to use forward slashes.
/// Fixes Windows backslash paths so glob matching works consistently.
pub fn normalize_path(file_path: &str) -> String {
    file_path.replace('\\', "/")
}

/// SHA-256 of arbitrary bytes, hex-encoded (the hash used for node IDs,
/// content hashes, and daemon paths throughout the codebase).
pub fn sha256_hex(content: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content);
    let out = hasher.finalize();
    let mut s = String::with_capacity(64);
    for b in out {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

// ============================================================
// FILE LOCK
// ============================================================

/// Cross-process file lock using a lock file with PID tracking.
///
/// Prevents multiple processes (e.g., git hooks, CLI, MCP server) from
/// writing to the same database simultaneously.
pub struct FileLock {
    lock_path: PathBuf,
    held: bool,
}

impl FileLock {
    /// Locks older than this are considered stale regardless of PID status.
    const STALE_TIMEOUT_MS: u128 = 2 * 60 * 1000; // 2 minutes

    pub fn new(lock_path: impl Into<PathBuf>) -> Self {
        FileLock {
            lock_path: lock_path.into(),
            held: false,
        }
    }

    /// Acquire the lock. Errors if the lock is held by another live process.
    pub fn acquire(&mut self) -> crate::error::Result<()> {
        use crate::error::CodeGraphError;

        if self.lock_path.exists() {
            let stale_or_invalid = (|| -> Option<bool> {
                let content = fs::read_to_string(&self.lock_path).ok()?;
                let pid: u32 = content.trim().parse().ok()?;
                let meta = fs::metadata(&self.lock_path).ok()?;
                let age = meta.modified().ok()?.elapsed().ok()?.as_millis();
                // Treat locks older than the timeout as stale, regardless of PID
                if age < Self::STALE_TIMEOUT_MS && is_process_alive(pid) {
                    Some(false) // live lock
                } else {
                    Some(true) // stale
                }
            })();

            match stale_or_invalid {
                Some(false) => {
                    let pid = fs::read_to_string(&self.lock_path)
                        .ok()
                        .and_then(|c| c.trim().parse::<u32>().ok())
                        .unwrap_or(0);
                    return Err(CodeGraphError::other(format!(
                        "CodeGraph database is locked by another process (PID {pid}). \
                         If this is stale, run 'codegraph unlock' or delete {}",
                        self.lock_path.display()
                    )));
                }
                // Stale lock (dead process or timed out) or unreadable - remove it
                _ => {
                    let _ = fs::remove_file(&self.lock_path);
                }
            }
        }

        // Write our PID to the lock file using exclusive create
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.lock_path)
        {
            Ok(mut f) => {
                use std::io::Write;
                let _ = write!(f, "{}", std::process::id());
                self.held = true;
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Race condition: another process grabbed the lock between check and write
                Err(CodeGraphError::other(format!(
                    "CodeGraph database is locked by another process. \
                     If this is stale, run 'codegraph unlock' or delete {}",
                    self.lock_path.display()
                )))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Release the lock.
    pub fn release(&mut self) {
        if !self.held {
            return;
        }
        // Only remove if we still own it (check PID)
        if let Ok(content) = fs::read_to_string(&self.lock_path) {
            if content.trim().parse::<u32>() == Ok(std::process::id()) {
                let _ = fs::remove_file(&self.lock_path);
            }
        }
        self.held = false;
    }

    /// Execute a function while holding the lock.
    pub fn with_lock<T>(&mut self, f: impl FnOnce() -> T) -> crate::error::Result<T> {
        self.acquire()?;
        let result = f();
        self.release();
        Ok(result)
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_is_alive() {
        assert!(is_process_alive(std::process::id()));
    }

    #[test]
    fn validate_path_within_root_blocks_traversal() {
        let root = Path::new("/home/user/project");
        assert!(validate_path_within_root(root, "src/main.rs").is_some());
        assert!(validate_path_within_root(root, "../../etc/passwd").is_none());
        assert!(validate_path_within_root(root, "a/../../../etc/passwd").is_none());
        // The root itself is allowed
        assert_eq!(
            validate_path_within_root(root, "."),
            Some(PathBuf::from("/home/user/project"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn validate_project_path_blocks_sensitive_dirs() {
        assert!(validate_project_path(Path::new("/etc")).is_some());
        assert!(validate_project_path(Path::new("/")).is_some());
    }

    #[test]
    fn clamp_works() {
        assert_eq!(clamp(5, 1, 10), 5);
        assert_eq!(clamp(-5, 1, 10), 1);
        assert_eq!(clamp(50, 1, 10), 10);
    }

    #[test]
    fn normalize_path_converts_backslashes() {
        assert_eq!(normalize_path("a\\b\\c.ts"), "a/b/c.ts");
    }

    #[test]
    fn sha256_hex_matches_known_vector() {
        // echo -n "" | sha256sum
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn file_lock_acquire_release_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join("test.lock");
        let mut lock = FileLock::new(&lock_path);
        lock.acquire().unwrap();
        assert!(lock_path.exists());

        // Second lock on same path must fail while held
        let mut lock2 = FileLock::new(&lock_path);
        assert!(lock2.acquire().is_err());

        lock.release();
        assert!(!lock_path.exists());

        // Now it can be acquired again
        let mut lock3 = FileLock::new(&lock_path);
        lock3.acquire().unwrap();
        lock3.release();
    }

    #[test]
    fn safe_json_parse_falls_back() {
        let v: Vec<String> = safe_json_parse("not json", vec![]);
        assert!(v.is_empty());
        let v: Vec<String> = safe_json_parse("[\"a\"]", vec![]);
        assert_eq!(v, vec!["a"]);
    }
}

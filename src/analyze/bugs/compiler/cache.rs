//! The compiler detector's result cache: one parsed run per project, keyed
//! by the lockfile's content, the sources' sizes and mtimes, and the lint
//! arguments. A build takes minutes; asking again with nothing changed
//! answers from here without starting cargo.

use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::diagnostic::Parsed;

/// Bump when what is cached (the parse, the lint table's meaning) changes.
const CACHE_VERSION: u32 = 2;
const FILE: &str = "compiler.cache.json";

/// Files outside the index that change what cargo builds.
const BUILD_FILES: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain",
    "rust-toolchain.toml",
    ".cargo/config.toml",
    ".cargo/config",
];

/// The key of a project's current state, and the newest mtime it saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    pub key: String,
    /// Newest source mtime (ms since the epoch; the lockfile, which cargo
    /// writes, left out): a run that started before it missed that edit.
    pub newest_ms: u64,
}

/// Fingerprint `root`: the lockfile by content; every indexed source and
/// build file by path, size and mtime; the driver arguments.
pub fn fingerprint(root: &Path, files: &[String], driver_args: &[String]) -> Fingerprint {
    let mut hasher = Sha256::new();
    hasher.update(CACHE_VERSION.to_le_bytes());
    for arg in driver_args {
        hasher.update(arg.as_bytes());
        hasher.update([0]);
    }
    if let Ok(lock) = fs::read(root.join("Cargo.lock")) {
        hasher.update(Sha256::digest(&lock));
    }
    let mut newest_ms = 0;
    let mut paths: Vec<&str> = files.iter().map(String::as_str).collect();
    paths.extend(BUILD_FILES);
    paths.sort_unstable();
    paths.dedup();
    for path in paths {
        let Ok(meta) = fs::metadata(root.join(path)) else {
            continue;
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .unwrap_or_default();
        // Cargo writes the lockfile itself (a first build, an offline
        // resolution): that is not an edit the run missed.
        if path != "Cargo.lock" {
            newest_ms = newest_ms.max(mtime.as_millis() as u64);
        }
        hasher.update(path.as_bytes());
        hasher.update([0]);
        hasher.update(meta.len().to_le_bytes());
        hasher.update(mtime.as_nanos().to_le_bytes());
    }
    Fingerprint {
        key: format!("{:x}", hasher.finalize()),
        newest_ms,
    }
}

/// A finished run as cached.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cached {
    version: u32,
    key: String,
    pub started_ms: u64,
    pub finished_ms: u64,
    pub parsed: Parsed,
    pub failure: Option<String>,
}

/// The cached run for `key`, if there is one.
pub fn load(dir: &Path, key: &str) -> Option<Cached> {
    let cached: Cached = serde_json::from_str(&fs::read_to_string(dir.join(FILE)).ok()?).ok()?;
    (cached.version == CACHE_VERSION && cached.key == key).then_some(cached)
}

/// Cache a finished run under `key` (best effort: a failed write only
/// costs a rebuild).
pub fn store(
    dir: &Path,
    key: &str,
    started_ms: u64,
    finished_ms: u64,
    parsed: &Parsed,
    failure: Option<&str>,
) {
    let cached = Cached {
        version: CACHE_VERSION,
        key: key.to_string(),
        started_ms,
        finished_ms,
        parsed: parsed.clone(),
        failure: failure.map(str::to_string),
    };
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    let Ok(bytes) = serde_json::to_vec(&cached) else {
        return;
    };
    let tmp = dir.join(format!("{FILE}.tmp"));
    if fs::write(&tmp, bytes).is_ok() {
        let _ = fs::rename(tmp, dir.join(FILE));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_follows_sources_lockfile_and_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "fn a() {}\n").unwrap();
        fs::write(root.join("Cargo.lock"), "version = 3\n").unwrap();
        let files = vec!["src/lib.rs".to_string()];
        let args = vec!["-W".to_string(), "clippy::eq_op".to_string()];
        let first = fingerprint(root, &files, &args);
        assert_eq!(first, fingerprint(root, &files, &args), "stable");
        assert!(first.newest_ms > 0);

        fs::write(root.join("src/lib.rs"), "fn a() { b() }\n").unwrap();
        let edited = fingerprint(root, &files, &args);
        assert_ne!(first.key, edited.key, "a source edit (size) changes it");

        fs::write(root.join("Cargo.lock"), "version = 4\n").unwrap();
        let locked = fingerprint(root, &files, &args);
        assert_ne!(edited.key, locked.key, "the lockfile's content changes it");

        assert_ne!(
            locked.key,
            fingerprint(root, &files, &[]).key,
            "and the lints"
        );
    }

    #[test]
    fn a_cached_run_is_found_only_under_its_key() {
        let dir = tempfile::tempdir().unwrap();
        let parsed = Parsed {
            errors: 2,
            ..Parsed::default()
        };
        store(dir.path(), "k1", 10, 20, &parsed, Some("boom"));
        let cached = load(dir.path(), "k1").unwrap();
        assert_eq!((cached.started_ms, cached.finished_ms), (10, 20));
        assert_eq!(cached.parsed.errors, 2);
        assert_eq!(cached.failure.as_deref(), Some("boom"));
        assert!(load(dir.path(), "k2").is_none());
    }
}

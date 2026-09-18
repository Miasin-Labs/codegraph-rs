//! The on-disk layout of the machine-wide dependency store and its locks.
//!
//! ```text
//! <codegraph_home>/deps/
//!   registry.db                      dependency versions, shard states, project usage
//!   <ecosystem>/<name>-<version>/    one shard: codegraph.db + meta.json
//!   <ecosystem>/.tmp-*/              a build in progress (renamed into place when done)
//!   .locks/<ecosystem>/<dir>.lock    per-shard build lock
//!   .locks/builder.lock              the one background builder
//! ```
//!
//! Only directories holding a `meta.json` (and the builder's `.tmp-`/`.old-`
//! directories) belong to the shard store; any other file under an
//! ecosystem directory — e.g. per-crate artifacts other passes write beside
//! the shards — is never touched.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::model::{DepKey, Ecosystem};

/// Prefix of a shard build's working directory.
pub(crate) const TMP_PREFIX: &str = ".tmp-";
/// Prefix of a replaced shard awaiting deletion.
pub(crate) const OLD_PREFIX: &str = ".old-";
/// The shard's description file.
pub const META_FILE: &str = "meta.json";

/// The machine-wide dependency store rooted at `codegraph_home()/deps`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepsHome {
    root: PathBuf,
}

impl DepsHome {
    /// `$CODEGRAPH_HOME/deps` (default `~/.codegraph/deps`).
    pub fn from_env() -> Self {
        Self::at(crate::directory::codegraph_home().join("deps"))
    }

    /// A store at an explicit directory (tests, tools).
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn registry_path(&self) -> PathBuf {
        self.root.join("registry.db")
    }

    pub fn ecosystem_dir(&self, ecosystem: Ecosystem) -> PathBuf {
        self.root.join(ecosystem.as_str())
    }

    pub fn shard_dir(&self, key: &DepKey) -> PathBuf {
        self.ecosystem_dir(key.ecosystem).join(key.dir_name())
    }

    pub fn shard_lock_path(&self, key: &DepKey) -> PathBuf {
        self.root
            .join(".locks")
            .join(key.ecosystem.as_str())
            .join(format!("{}.lock", key.dir_name()))
    }

    pub fn builder_lock_path(&self) -> PathBuf {
        self.root.join(".locks").join("builder.lock")
    }

    /// Create the store (owner-only on unix; its parent too when that is
    /// the CodeGraph home).
    pub fn ensure(&self) -> io::Result<()> {
        if let Some(parent) = self.root.parent() {
            if parent == crate::directory::codegraph_home() {
                crate::directory::ensure_codegraph_home()
                    .map_err(|e| io::Error::other(e.to_string()))?;
            }
        }
        fs::create_dir_all(&self.root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
}

/// An exclusive OS file lock (`flock`), released when dropped or when the
/// holding process dies — a crashed builder never leaves a shard locked.
#[derive(Debug)]
pub struct StoreLock {
    _file: File,
}

impl StoreLock {
    /// Take the lock at `path` without waiting; `Ok(None)` when another
    /// process (or another handle in this one) holds it.
    pub fn try_acquire(path: &Path) -> io::Result<Option<StoreLock>> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;
        match file.try_lock() {
            Ok(()) => {
                // Diagnostics only: who holds it.
                let _ = file.set_len(0);
                let _ = write!(file, "{}", std::process::id());
                Ok(Some(StoreLock { _file: file }))
            }
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(error),
        }
    }

    /// Whether a live process holds the lock at `path`. Never creates the
    /// file: a missing lock file is an unheld lock.
    pub fn is_held(path: &Path) -> bool {
        let Ok(file) = OpenOptions::new().read(true).open(path) else {
            return false;
        };
        match file.try_lock_shared() {
            Ok(()) => {
                let _ = file.unlock();
                false
            }
            Err(TryLockError::WouldBlock) => true,
            Err(TryLockError::Error(_)) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_lock_excludes_other_handles_until_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("locks/x.lock");
        assert!(!StoreLock::is_held(&path));
        let first = StoreLock::try_acquire(&path).unwrap().expect("free lock");
        assert!(StoreLock::is_held(&path));
        assert!(StoreLock::try_acquire(&path).unwrap().is_none());
        drop(first);
        assert!(!StoreLock::is_held(&path));
        assert!(StoreLock::try_acquire(&path).unwrap().is_some());
    }

    #[test]
    fn layout() {
        let home = DepsHome::at("/h/deps");
        let key = DepKey::new(Ecosystem::Crates, "serde", "1.0.219");
        assert_eq!(
            home.shard_dir(&key),
            PathBuf::from("/h/deps/crates/serde-1.0.219")
        );
        assert_eq!(
            home.shard_lock_path(&key),
            PathBuf::from("/h/deps/.locks/crates/serde-1.0.219.lock")
        );
        assert_eq!(home.registry_path(), PathBuf::from("/h/deps/registry.db"));
    }
}

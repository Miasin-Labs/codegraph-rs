//! Lockfiles → resolved dependencies.
//!
//! [`discover`] finds a project's lockfiles (the root and a few levels of
//! subdirectories, never inside dependency or build trees); [`read_project`]
//! parses them all. Parsing never touches the network or the source caches —
//! locating sources is [`crate::deps::locate`]'s job.

mod bun;
mod cargo;
mod go;
mod js;
mod npm;
mod pnpm;
mod yarn;

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::model::{Ecosystem, ResolvedDep};

/// Which lockfile format a file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LockfileKind {
    CargoLock,
    PackageLock,
    PnpmLock,
    BunLock,
    YarnLock,
    GoMod,
}

impl LockfileKind {
    pub fn ecosystem(self) -> Ecosystem {
        match self {
            Self::CargoLock => Ecosystem::Crates,
            Self::PackageLock | Self::PnpmLock | Self::BunLock | Self::YarnLock => Ecosystem::Npm,
            Self::GoMod => Ecosystem::Go,
        }
    }

    /// Detect a lockfile by file name. JS directories may hold several;
    /// [`discover`] keeps the first of npm, pnpm, bun, yarn per directory.
    fn from_file_name(name: &str) -> Option<Self> {
        match name {
            "Cargo.lock" => Some(Self::CargoLock),
            "package-lock.json" | "npm-shrinkwrap.json" => Some(Self::PackageLock),
            "pnpm-lock.yaml" => Some(Self::PnpmLock),
            "bun.lock" => Some(Self::BunLock),
            "yarn.lock" => Some(Self::YarnLock),
            "go.mod" => Some(Self::GoMod),
            _ => None,
        }
    }

    fn js_rank(self) -> Option<u8> {
        match self {
            Self::PackageLock => Some(0),
            Self::PnpmLock => Some(1),
            Self::BunLock => Some(2),
            Self::YarnLock => Some(3),
            _ => None,
        }
    }
}

/// One lockfile of a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Lockfile {
    pub kind: LockfileKind,
    /// Path relative to the project root (`/`-separated).
    pub rel_path: String,
    #[serde(skip)]
    pub path: PathBuf,
}

impl Lockfile {
    /// Directory holding the lockfile, relative to the project root (`""`
    /// for the root itself).
    pub fn rel_dir(&self) -> &str {
        self.rel_path.rsplit_once('/').map_or("", |(dir, _)| dir)
    }
}

/// Directories never searched for lockfiles: dependency trees, build
/// output, VCS metadata, and test fixtures (which often carry lockfiles of
/// their own that are not the project's).
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "vendor",
    "dist",
    "build",
    "out",
    "testdata",
    "fixtures",
    "__fixtures__",
    "third_party",
    "bower_components",
];

/// How deep below the root lockfiles are looked for.
const MAX_DEPTH: usize = 3;
/// Directory entries a discovery walk may visit.
const MAX_ENTRIES: usize = 20_000;

/// The lockfiles of the project at `root`, sorted by path.
pub fn discover(root: &Path) -> Vec<Lockfile> {
    let mut found: Vec<Lockfile> = Vec::new();
    let walker = walkdir::WalkDir::new(root)
        .max_depth(MAX_DEPTH)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            if entry.depth() == 0 || !entry.file_type().is_dir() {
                return true;
            }
            let name = entry.file_name().to_string_lossy();
            !(name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()))
        });
    for entry in walker.take(MAX_ENTRIES).flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let Some(kind) = LockfileKind::from_file_name(&entry.file_name().to_string_lossy()) else {
            continue;
        };
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        let rel_path = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        found.push(Lockfile {
            kind,
            rel_path,
            path: entry.path().to_path_buf(),
        });
    }
    found.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

    // One JS lockfile per directory: the package manager actually in use
    // leaves its own, but stale ones from another manager linger.
    let mut kept: Vec<Lockfile> = Vec::new();
    for lock in found {
        if let Some(rank) = lock.kind.js_rank() {
            if let Some(existing) = kept
                .iter_mut()
                .find(|k| k.kind.js_rank().is_some() && k.rel_dir() == lock.rel_dir())
            {
                if rank < existing.kind.js_rank().unwrap_or(u8::MAX) {
                    *existing = lock;
                }
                continue;
            }
        }
        kept.push(lock);
    }
    kept
}

/// A project's dependencies as its lockfiles resolve them.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectManifest {
    pub lockfiles: Vec<Lockfile>,
    pub deps: Vec<ResolvedDep>,
    /// Lockfiles that could not be read or parsed, with the reason.
    pub errors: Vec<String>,
}

/// Parse every lockfile of the project at `root`.
pub fn read_project(root: &Path) -> ProjectManifest {
    let lockfiles = discover(root);
    let mut manifest = ProjectManifest {
        lockfiles: lockfiles.clone(),
        ..ProjectManifest::default()
    };
    for lock in &lockfiles {
        match parse_lockfile(root, lock) {
            Ok(deps) => manifest.deps.extend(deps),
            Err(error) => manifest.errors.push(error),
        }
    }
    manifest
}

/// Parse one lockfile.
pub fn parse_lockfile(root: &Path, lock: &Lockfile) -> Result<Vec<ResolvedDep>, String> {
    let text = fs::read_to_string(&lock.path).map_err(|e| format!("{}: {e}", lock.rel_path))?;
    let dir = lock.rel_dir();
    let abs_dir = lock.path.parent().unwrap_or(root);
    match lock.kind {
        LockfileKind::CargoLock => {
            let members = cargo::workspace_members(abs_dir);
            Ok(cargo::parse(&text, &lock.rel_path, &members))
        }
        LockfileKind::PackageLock => {
            let declared = js::package_json_dependency_names(abs_dir);
            npm::parse(&text, &lock.rel_path, dir, &declared)
        }
        LockfileKind::PnpmLock => Ok(pnpm::parse(&text, &lock.rel_path)),
        LockfileKind::BunLock => bun::parse(&text, &lock.rel_path, dir),
        LockfileKind::YarnLock => {
            let declared: HashSet<String> = js::package_json_dependency_names(abs_dir);
            Ok(yarn::parse(&text, &lock.rel_path, &declared))
        }
        LockfileKind::GoMod => {
            let sum = fs::read_to_string(abs_dir.join("go.sum")).ok();
            Ok(go::parse(&text, sum.as_deref(), &lock.rel_path))
        }
    }
}

/// A cheap fingerprint of a project's lockfiles (paths, sizes, mtimes, plus
/// `go.sum` beside a `go.mod`): equal fingerprints mean nothing to re-parse.
pub fn fingerprint(lockfiles: &[Lockfile]) -> String {
    let mut hasher = Sha256::new();
    for lock in lockfiles {
        let mut files = vec![lock.path.clone()];
        if lock.kind == LockfileKind::GoMod {
            files.push(lock.path.with_file_name("go.sum"));
        }
        for file in files {
            hasher.update(file.to_string_lossy().as_bytes());
            if let Ok(meta) = fs::metadata(&file) {
                hasher.update(meta.len().to_le_bytes());
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_nanos());
                hasher.update(mtime.to_le_bytes());
            }
            hasher.update([0u8]);
        }
    }
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_nested_lockfiles_and_skips_dependency_trees() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for path in [
            "Cargo.lock",
            "web/package-lock.json",
            "web/yarn.lock",
            "tools/go.mod",
            "node_modules/x/package-lock.json",
            "target/debug/Cargo.lock",
            ".hidden/Cargo.lock",
            "tests/fixtures/app/Cargo.lock",
            "a/b/c/d/Cargo.lock",
        ] {
            let file = root.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, "").unwrap();
        }
        let found: Vec<(LockfileKind, String)> = discover(root)
            .into_iter()
            .map(|l| (l.kind, l.rel_path))
            .collect();
        assert_eq!(
            found,
            vec![
                (LockfileKind::CargoLock, "Cargo.lock".to_string()),
                (LockfileKind::GoMod, "tools/go.mod".to_string()),
                (
                    LockfileKind::PackageLock,
                    "web/package-lock.json".to_string()
                ),
            ]
        );
    }

    #[test]
    fn fingerprint_changes_with_the_lockfile() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Cargo.lock"), "version = 4\n").unwrap();
        let locks = discover(dir.path());
        let before = fingerprint(&locks);
        assert_eq!(before, fingerprint(&locks));
        fs::write(dir.path().join("Cargo.lock"), "version = 4\n\n").unwrap();
        assert_ne!(before, fingerprint(&locks));
    }
}

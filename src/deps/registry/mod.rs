//! `registry.db`: which dependency versions exist on this machine, the state
//! of each one's shard, and which projects use them.
//!
//! Writers ([`Registry::open`]) create the file owner-only (0600) in WAL
//! mode and migrate it; readers ([`Registry::open_read_only`]) never create
//! or change anything. The shards' own `meta.json` stays the source of
//! truth for what a shard holds; the registry is the index over them (for
//! listing, scheduling builds, and garbage collection).

mod read;
mod schema;
mod write;

use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{fs, io};

use rusqlite::{Connection, OpenFlags};
use serde::Serialize;

pub use self::read::{PendingScope, RegistryStatus, StateCount};
use super::error::DepsResult;
use super::model::{DepKey, DepSource, Ecosystem, ShardState, SourceKind};

/// The dependency registry.
pub struct Registry {
    conn: Connection,
}

impl Registry {
    /// Open for writing, creating the file (0600) and its directory when
    /// absent, and migrating it to the current schema.
    pub fn open(path: &Path) -> DepsResult<Self> {
        create_private(path)?;
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = NORMAL;")?;
        schema::migrate(&mut conn)?;
        Ok(Self { conn })
    }

    /// Open an existing registry read-only; `Ok(None)` when there is none
    /// yet (or it predates this build's schema). Never creates anything.
    pub fn open_read_only(path: &Path) -> DepsResult<Option<Self>> {
        if !path.is_file() {
            return Ok(None);
        }
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(Duration::from_millis(500))?;
        let version = schema::user_version(&conn)?;
        Ok((version >= schema::CURRENT_VERSION).then_some(Self { conn }))
    }

    /// The registry's schema version (`PRAGMA user_version`).
    pub fn schema_version(&self) -> DepsResult<i64> {
        Ok(schema::user_version(&self.conn)?)
    }
}

fn create_private(path: &Path) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// A resolved dependency plus where its source was found on this machine.
#[derive(Debug, Clone)]
pub struct LocatedDep {
    pub dep: super::model::ResolvedDep,
    pub source_dir: Option<PathBuf>,
}

/// One dependency version, its shard state, and how many projects use it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageRow {
    #[serde(skip)]
    pub id: i64,
    pub ecosystem: Ecosystem,
    pub name: String,
    pub version: String,
    pub source: DepSource,
    pub state: ShardState,
    pub shard_bytes: u64,
    pub files: Option<u64>,
    pub nodes: Option<u64>,
    pub edges: Option<u64>,
    pub build_ms: Option<u64>,
    pub built_at: Option<i64>,
    pub extractor_version: Option<u32>,
    pub partial_reasons: Option<String>,
    pub error: Option<String>,
    pub built_from: Option<String>,
    pub first_seen: i64,
    pub last_used: i64,
    /// Projects whose lockfiles pin this version.
    pub users: u64,
}

impl PackageRow {
    pub fn key(&self) -> DepKey {
        DepKey::new(self.ecosystem, self.name.clone(), self.version.clone())
    }
}

/// One dependency of one project, as `dependencies_of` returns it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDependency {
    pub key: DepKey,
    pub source: DepSource,
    /// `Some(true)` direct, `Some(false)` transitive, `None` unknown.
    pub direct: Option<bool>,
    /// The lockfile that pinned it, relative to the project root.
    pub lockfile: String,
    /// Where its source is on this machine, when found.
    pub source_dir: Option<String>,
    pub state: ShardState,
}

/// A recorded project.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRow {
    pub root: String,
    pub lockfiles: u64,
    pub recorded_at: i64,
    pub dependencies: u64,
}

/// A shard that should be (re)built, with every known copy of its source.
#[derive(Debug, Clone)]
pub struct PendingShard {
    pub key: DepKey,
    pub source: DepSource,
    pub state: ShardState,
    /// Located source directories, the requesting project's first.
    pub source_dirs: Vec<PathBuf>,
    pub direct: bool,
    pub users: u64,
}

/// What recording a project did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordSummary {
    pub dependencies: usize,
    pub located: usize,
    pub unavailable: usize,
    pub path: usize,
}

pub(crate) fn source_from_columns(
    kind: &str,
    url: Option<String>,
    rev: Option<String>,
) -> DepSource {
    match SourceKind::parse(kind) {
        Some(SourceKind::Git) => DepSource::Git {
            url: url.unwrap_or_default(),
            rev: rev.unwrap_or_default(),
        },
        Some(SourceKind::Path) => DepSource::Path { path: url },
        _ => DepSource::Registry,
    }
}

pub(crate) fn source_columns(source: &DepSource) -> (&'static str, Option<&str>, Option<&str>) {
    match source {
        DepSource::Registry => ("registry", None, None),
        DepSource::Git { url, rev } => ("git", Some(url.as_str()), Some(rev.as_str())),
        DepSource::Path { .. } => ("path", None, None),
    }
}

#[cfg(test)]
mod tests;

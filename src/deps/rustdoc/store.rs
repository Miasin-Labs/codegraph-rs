//! API indexes on disk: `api/<crate>.json` inside the shard directory,
//! written into the shard's `.tmp-` build directory (so they publish with
//! it, atomically), read-only afterwards.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use super::API_FORMAT;
use super::model::ApiIndex;

/// The directory inside a shard holding its API indexes.
pub const API_DIR: &str = "api";

/// What a shard's `meta.json` records about its API indexes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiMeta {
    /// [`API_FORMAT`] of the indexes (0: none were built).
    #[serde(default)]
    pub format: u32,
    /// The rustdoc JSON `format_version` they were read from.
    #[serde(default)]
    pub rustdoc_format: u32,
    /// The crates indexed (`std`, `core`, `alloc`; or the crate's name).
    #[serde(default)]
    pub crates: Vec<String>,
    /// Where the JSON came from: `toolchain` (the `rust-docs-json`
    /// component) or `cargo-rustdoc`.
    #[serde(default)]
    pub origin: String,
    /// Shard nodes the indexes renamed (a method the grammar hoisted to a
    /// bare function) and added (an item the grammar lost).
    #[serde(default)]
    pub renamed: usize,
    #[serde(default)]
    pub added: usize,
    /// Why no index was built this time (kept so a failing crate is not
    /// retried on every build).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ApiMeta {
    /// Indexes of the current format are present.
    pub fn is_current(&self) -> bool {
        self.format == API_FORMAT && !self.crates.is_empty()
    }
}

/// Where the index of `krate` is inside the shard `dir`.
pub fn artifact_path(dir: &Path, krate: &str) -> PathBuf {
    dir.join(API_DIR).join(format!("{krate}.json"))
}

/// Write the index into the (unpublished) shard directory `dir`: 0600,
/// through a temporary file renamed into place.
pub fn write(dir: &Path, index: &ApiIndex) -> std::io::Result<()> {
    let target = artifact_path(dir, &index.krate);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = target.with_extension("json.tmp");
    {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = std::io::BufWriter::new(options.open(&tmp)?);
        serde_json::to_writer(&mut file, index)?;
        file.flush()?;
    }
    std::fs::rename(&tmp, &target)
}

type Cache = Mutex<HashMap<PathBuf, (Option<SystemTime>, Option<Arc<ApiIndex>>)>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The index of `krate` in the shard `dir`, read once per process (and
/// again when the file changes) — `None` when there is none, or it is of
/// another format. Opens the file read-only; creates nothing.
pub fn load(dir: &Path, krate: &str) -> Option<Arc<ApiIndex>> {
    let path = artifact_path(dir, krate);
    let modified = std::fs::metadata(&path)
        .ok()
        .and_then(|m| m.modified().ok());
    modified?;
    if let Ok(cache) = cache().lock() {
        if let Some((when, index)) = cache.get(&path) {
            if *when == modified {
                return index.clone();
            }
        }
    }
    let index = read(&path).map(Arc::new);
    if let Ok(mut cache) = cache().lock() {
        cache.insert(path, (modified, index.clone()));
    }
    index
}

fn read(path: &Path) -> Option<ApiIndex> {
    let bytes = std::fs::read(path).ok()?;
    let index: ApiIndex = serde_json::from_slice(&bytes).ok()?;
    (index.format == API_FORMAT).then_some(index)
}

//! The summaries artifact: one JSON file inside the shard directory it
//! describes (`<shard>/taint-summaries.json`), so it lives and dies with
//! the shard — a rebuild publishes a new directory without one, `gc`
//! removes it with the shard — and a reader checks its stamp against the
//! shard's `meta.json` before trusting it.
//!
//! Written only by the CLI (`deps build`, or `analyze rules` on first
//! need) under the shard's build lock, to a temporary file renamed into
//! place (0600); read by anyone, never written by readers.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::deps::shard::ShardMeta;

/// Bump whenever the Rust lowering, the taint engine or what a summary
/// records changes: older artifacts are then rebuilt, never read.
pub const SUMMARY_VERSION: u32 = 3;
/// The artifact's `format`.
pub const FORMAT: &str = "codegraph-dep-summaries";
/// The artifact's file name inside the shard directory.
pub const FILE_NAME: &str = "taint-summaries.json";

/// Which shard build an artifact was computed from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShardStamp {
    pub key: String,
    pub built_at_ms: i64,
    pub source_fingerprint: String,
    pub extractor_version: u32,
}

impl ShardStamp {
    pub fn of(meta: &ShardMeta) -> Self {
        Self {
            key: meta.key().to_string(),
            built_at_ms: meta.built_at_ms,
            source_fingerprint: meta.source_fingerprint.clone(),
            extractor_version: meta.extractor_version,
        }
    }
}

/// A function the artifact names (summarized, or a step of a path).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredFunction {
    /// The shard's node id.
    pub id: String,
    /// Qualified name as the shard records it (`Client::get`).
    pub q: String,
    /// File, relative to the shard's source directory.
    pub f: String,
    /// 1-based line where it starts.
    pub l: u32,
    /// Its positional parameters.
    #[serde(default)]
    pub a: u32,
    /// It takes `self`.
    #[serde(default)]
    pub r: bool,
    /// It (or what it calls) uses `unsafe`: an empty return may be a flow
    /// the IR lost, so a reader assumes the return carries its inputs.
    #[serde(default)]
    pub u: bool,
}

/// An input or output place: a slot (`self`, `p<i>`, `g:<key>`, `return`)
/// and the field path below it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StoredAccess {
    pub slot: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
}

/// One step of a path: a function of the artifact and a line in it.
pub type StoredStep = (u32, u32);

/// An input that reaches an output, with its path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredInput {
    pub input: StoredAccess,
    pub path: Vec<StoredStep>,
}

/// An output's facts: the inputs it carries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredOutput {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<StoredInput>,
}

/// A call the shard does not resolve (another crate's, std's) and what
/// reaches it: `input` reaches argument `arg` of `callee`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCallArg {
    pub input: StoredAccess,
    /// The callee as written (`std::fs::read`, `Command::new`).
    pub callee: String,
    pub arg: u32,
    pub path: Vec<StoredStep>,
}

/// The environment (`env::var(..)`, as written: `callee`) reaching an
/// output (`return`, `return.<field>`, `self.<field>`, `p<i>`…).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredEnvironment {
    pub output: StoredAccess,
    pub callee: String,
    pub path: Vec<StoredStep>,
}

/// One function's summary.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredSummary {
    /// Index into [`Artifact::functions`].
    pub function: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub returns: Vec<StoredInput>,
    /// Per returned field, when narrower than the whole.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<(String, StoredOutput)>,
    /// Storage callers see written: target, and what it then holds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writes: Vec<(StoredAccess, StoredOutput)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<StoredCallArg>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub environment: Vec<StoredEnvironment>,
}

/// The summaries of one shard's functions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    pub format: String,
    pub version: u32,
    pub shard: ShardStamp,
    /// Every function was analyzed (no budget ran out).
    pub complete: bool,
    pub functions_total: usize,
    pub functions_lowered: usize,
    pub elapsed_ms: u64,
    pub functions: Vec<StoredFunction>,
    pub summaries: Vec<StoredSummary>,
}

/// Where the artifact of the shard in `shard_dir` lives.
pub fn artifact_path(shard_dir: &Path) -> PathBuf {
    shard_dir.join(FILE_NAME)
}

/// The artifact of the shard in `shard_dir`, when it is current for the
/// shard `meta` describes and this build's [`SUMMARY_VERSION`].
pub fn read(shard_dir: &Path, meta: &ShardMeta) -> Option<Artifact> {
    let text = fs::read_to_string(artifact_path(shard_dir)).ok()?;
    let artifact: Artifact = serde_json::from_str(&text).ok()?;
    (artifact.format == FORMAT
        && artifact.version == SUMMARY_VERSION
        && artifact.shard == ShardStamp::of(meta))
    .then_some(artifact)
}

/// Write `artifact` into `shard_dir`, atomically and owner-only.
pub fn write(shard_dir: &Path, artifact: &Artifact) -> io::Result<()> {
    let text = serde_json::to_string(artifact).map_err(io::Error::other)?;
    let path = artifact_path(shard_dir);
    let temp = shard_dir.join(format!(
        ".{FILE_NAME}.{}.{:x}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = options.open(&temp).and_then(|mut file| {
        file.write_all(text.as_bytes())?;
        file.sync_all()
    });
    if let Err(error) = written.and_then(|()| fs::rename(&temp, &path)) {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    Ok(())
}

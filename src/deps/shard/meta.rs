//! `meta.json`: what a shard is, what it was built from and with, and what
//! it holds. The shard's own description — readers trust it, not the
//! registry, so a shard stays usable even if the registry is lost.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::db::CURRENT_SCHEMA_VERSION;
use crate::deps::model::{DepKey, DepSource, Ecosystem, ShardState};
use crate::deps::scope::{PartialReason, ShardLimits};
use crate::deps::store::META_FILE;
use crate::extraction::EXTRACTION_VERSION;

/// Version of the `meta.json` layout itself.
pub const META_FORMAT: u32 = 1;

/// What a shard holds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShardCounts {
    /// Library files found in the source before any budget.
    pub candidate_files: usize,
    pub candidate_bytes: u64,
    /// Files chosen within the budgets.
    pub selected_files: usize,
    pub selected_bytes: u64,
    /// Files the extractor stored (0-symbol files included).
    pub indexed_files: usize,
    pub errored_files: usize,
    /// Files skipped for exceeding the per-file size cap.
    pub oversized_files: usize,
    pub nodes: u64,
    pub edges: u64,
}

/// The contents of a shard's `meta.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShardMeta {
    pub format: u32,
    pub ecosystem: Ecosystem,
    pub name: String,
    /// The shard key's version (see [`DepKey`]).
    pub version: String,
    pub source: DepSource,
    /// The (read-only) source tree node file paths are relative to.
    pub source_dir: String,
    /// Hash of the library files' paths and sizes ([`crate::deps::scope`]).
    pub source_fingerprint: String,
    pub source_mtime_ms: i64,
    pub extractor_version: u32,
    pub schema_version: u32,
    pub codegraph_version: String,
    pub built_at_ms: i64,
    pub build_ms: u64,
    /// `ready` or `partial`.
    pub state: ShardState,
    pub partial_reasons: Vec<PartialReason>,
    pub limits: ShardLimits,
    pub counts: ShardCounts,
    /// Size of `codegraph.db`.
    pub db_bytes: u64,
}

impl ShardMeta {
    pub fn key(&self) -> DepKey {
        DepKey::new(self.ecosystem, self.name.clone(), self.version.clone())
    }

    /// Built by this extractor and schema (an older shard still reads —
    /// same schema — but misses newer extraction; it is rebuilt lazily).
    pub fn is_current(&self) -> bool {
        self.extractor_version >= EXTRACTION_VERSION
            && self.schema_version == CURRENT_SCHEMA_VERSION
    }

    /// The database can be opened by this build without migrating it.
    pub fn is_readable(&self) -> bool {
        self.format == META_FORMAT && self.schema_version == CURRENT_SCHEMA_VERSION
    }

    pub fn read(shard_dir: &Path) -> Option<ShardMeta> {
        let text = fs::read_to_string(shard_dir.join(META_FILE)).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn write(&self, shard_dir: &Path) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        fs::write(shard_dir.join(META_FILE), text)
    }
}

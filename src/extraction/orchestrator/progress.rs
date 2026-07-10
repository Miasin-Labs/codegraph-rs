use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::types::{ExtractionError, Severity};

/// Epoch milliseconds (`Date.now()` parity).
pub(super) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Progress phase for indexing operations (TS string union).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexPhase {
    Scanning,
    Parsing,
    Storing,
    Resolving,
}

impl IndexPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            IndexPhase::Scanning => "scanning",
            IndexPhase::Parsing => "parsing",
            IndexPhase::Storing => "storing",
            IndexPhase::Resolving => "resolving",
        }
    }
}

/// Progress callback payload for indexing operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexProgress {
    pub phase: IndexPhase,
    pub current: usize,
    pub total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_file: Option<String>,
}

/// Result of an indexing operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexResult {
    pub success: bool,
    pub files_indexed: usize,
    pub files_skipped: usize,
    pub files_errored: usize,
    /// Number of indexable files found by a full project scan. `None` for
    /// targeted `index_files` runs, where there is no scan ground truth.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files_discovered: Option<usize>,
    pub nodes_created: usize,
    pub edges_created: usize,
    pub errors: Vec<ExtractionError>,
    pub duration_ms: i64,
}

/// Result of a sync operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncResult {
    pub files_checked: usize,
    pub files_added: usize,
    pub files_modified: usize,
    pub files_removed: usize,
    pub nodes_updated: usize,
    pub duration_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_file_paths: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_node_names: Option<Vec<String>>,
}

/// Return shape of [`ExtractionOrchestrator::get_changed_files`]
/// (anonymous object in TS).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangedFiles {
    pub added: Vec<String>,
    pub modified: Vec<String>,
    pub removed: Vec<String>,
}

/// Return shape of [`ExtractionOrchestrator::reconcile_removed_files`]
/// (anonymous object in TS).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconcileResult {
    pub files_removed: usize,
    pub removed_node_names: Vec<String>,
}

/// The subset of `fs.Stats` the orchestrator consumes (size + mtime).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStats {
    /// File size in bytes (`stats.size`).
    pub size: u64,
    /// Modification time in epoch milliseconds (`stats.mtimeMs`, floored).
    pub modified_at_ms: i64,
}

impl FileStats {
    pub fn from_metadata(meta: &fs::Metadata) -> Self {
        FileStats {
            size: meta.len(),
            modified_at_ms: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
        }
    }
}

pub(super) fn aborted_error() -> ExtractionError {
    ExtractionError {
        message: "Aborted".to_string(),
        file_path: None,
        line: None,
        column: None,
        severity: Severity::Error,
        code: None,
    }
}

pub(super) fn emit(on_progress: Option<&dyn Fn(&IndexProgress)>, progress: IndexProgress) {
    if let Some(cb) = on_progress {
        cb(&progress);
    }
}

pub(super) fn is_aborted(signal: Option<&AtomicBool>) -> bool {
    signal.is_some_and(|s| s.load(Ordering::Relaxed))
}

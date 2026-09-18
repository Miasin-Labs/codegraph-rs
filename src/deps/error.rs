//! Errors from the dependency store.

/// Errors from the registry, the shard store, or a shard build.
#[derive(Debug, thiserror::Error)]
pub enum DepsError {
    #[error("dependency registry: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("dependency store I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("dependency metadata: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}

pub type DepsResult<T> = Result<T, DepsError>;

//! Tool-call history flywheel: a global, redacted store of agent tool usage.
//!
//! Distinct from the per-project code graph in [`crate::db`]: tool history spans
//! every project, must survive re-indexing, and records *behavior* (which files
//! get read/edited, which commands run, where the agent falls back to grep) —
//! so it lives in its own SQLite database (default `~/.codegraph/history.db`),
//! never entangled with the graph schema.
//!
//! What it powers (queried by the CLI / future MCP layer):
//! * **hot files & tools** — ranking priors for exploration;
//! * **co-access** — files read together in a session (coupling the static call
//!   graph misses);
//! * **command profile** — the grep/cargo/git chains the user actually runs.
//!
//! Pipeline: a [`ToolCallSource`] adapter (one per agent, see [`sources`])
//! reads the agent's native store and emits one [`RawToolCall`] per native
//! call id. [`ToolEvent::from_raw`] — the only way to build a row — redacts
//! every string ([`redact`]) before deriving the command profile and project
//! from it. [`HistoryDb`] stores rows keyed on a hash of the native id, so
//! re-ingesting is idempotent.

#![forbid(unsafe_code)]

mod event;
mod project;
pub mod redact;
mod schema;
mod shell;
pub mod sources;
mod store;

use std::path::PathBuf;

pub use event::{RawToolCall, ToolEvent, call_key};
pub use project::ProjectResolver;
pub use redact::redact;
pub use sources::jfc::JfcLogs;
pub use sources::{SourceStats, ToolCallSource};
pub use store::{HistoryDb, HistoryError, IngestOptions, IngestReport};

/// The default on-disk location: `~/.codegraph/history.db`.
pub fn default_history_path() -> PathBuf {
    home_dir().join(".codegraph").join("history.db")
}

/// Default JFC log directory to ingest from: `~/.config/jfc/logs`.
pub fn default_jfc_logs_dir() -> PathBuf {
    home_dir().join(".config").join("jfc").join("logs")
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

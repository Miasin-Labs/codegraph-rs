//! Tool-call history flywheel: a global, redacted store of agent tool usage,
//! and the cross-session memory built on it.
//!
//! Distinct from the per-project code graph in [`crate::db`]: tool history spans
//! every project, must survive re-indexing, and records *behavior* (which files
//! get read/edited, which commands run, where the agent falls back to grep) —
//! so it lives in its own SQLite database (default `~/.codegraph/history.db`,
//! `CODEGRAPH_HISTORY_DB` overrides), never entangled with the graph schema.
//!
//! What it powers (queried by the CLI and MCP):
//! * **hot files & tools** — ranking priors for exploration;
//! * **co-access** — files read together in a session (coupling the static call
//!   graph misses);
//! * **command profile** — the grep/cargo/git chains the user actually runs;
//! * **cross-session memory** ([`memory`]) — what earlier sessions in a
//!   repository explored, looked up, broke and fixed: `codegraph history
//!   recall`, MCP `codegraph_recall`, and the prompt hook's session digest.
//!
//! Pipeline: a [`ToolCallSource`] / [`EventSource`] adapter (one per agent, see
//! [`sources`]) reads the agent's native store and emits one [`RawToolCall`] per
//! native call id (plus sessions and human prompts). [`ToolEvent::from_raw`] —
//! the only way to build a row — redacts every string ([`redact`]) before
//! deriving the command profile and project from it; the memory writer
//! redacts again and keeps only repo-relative paths, index-resolved
//! identifiers (else hashes), masked command templates, codes and numbers.
//! Rows are keyed on a hash of the native id, so re-ingesting is idempotent.

#![forbid(unsafe_code)]

mod activity;
pub mod background;
mod event;
pub mod ingest;
pub mod memory;
mod project;
pub mod redact;
mod repo;
mod schema;
pub mod session_start;
mod shell;
pub mod sources;
mod store;
mod time;

use std::path::{Path, PathBuf};

pub use event::{CallResult, RawToolCall, ToolEvent, call_key};
pub use ingest::{IncrementalOptions, IncrementalReport, SourceReport};
pub use project::ProjectResolver;
pub use redact::redact;
pub use sources::claude_code::ClaudeCodeProjects;
pub use sources::jfc::JfcLogs;
pub use sources::opencode::OpencodeDb;
pub use sources::{EventSource, SourceStats, ToolCallSource};
pub use store::{HistoryDb, HistoryError, IngestOptions, IngestReport, ensure_store_dir};

/// The history store: `CODEGRAPH_HISTORY_DB`, else `~/.codegraph/history.db`.
pub fn default_history_path() -> PathBuf {
    match std::env::var_os("CODEGRAPH_HISTORY_DB") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => home_dir().join(".codegraph").join("history.db"),
    }
}

/// Default JFC log directory to ingest from: `~/.config/jfc/logs`.
pub fn default_jfc_logs_dir() -> PathBuf {
    home_dir().join(".config").join("jfc").join("logs")
}

/// Canonical git repository containing `dir` (worktrees fold into their
/// main checkout), as the memory keys repositories.
pub fn repo_root_of(dir: &Path) -> Option<PathBuf> {
    repo::RepoLocator::new().repo_of_dir(dir)
}

/// Parse a duration like `30m`, `12h`, `7d`, `2w` (a bare number is days)
/// into milliseconds.
pub fn parse_duration_ms(s: &str) -> Option<i64> {
    time::parse_duration_ms(s)
}

/// Current wall-clock time in epoch milliseconds.
pub fn now_ms() -> i64 {
    time::now_ms()
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

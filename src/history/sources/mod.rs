//! Source adapters: one module per agent whose tool calls we ingest.
//!
//! An adapter turns an agent's native store (logs, a session DB, JSONL
//! transcripts) into [`RawToolCall`]s — exactly one per native call id —
//! and nothing else: redaction, project attribution and storage are shared
//! (see [`super::HistoryDb::ingest_source`]). Adding an agent means adding a
//! module here that implements [`ToolCallSource`].

pub mod jfc;

use super::event::RawToolCall;
use super::store::HistoryError;

/// Counters an adapter reports about one pass over its store.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceStats {
    /// Input units read (log files, sessions, …).
    pub inputs: usize,
    /// Input units that could not be read.
    pub skipped: usize,
    /// Tool calls emitted.
    pub calls: usize,
}

/// An agent-specific reader of tool calls.
pub trait ToolCallSource {
    /// Short, stable source id (`jfc`). Stored on every row and mixed into
    /// its dedupe key, so it must never change for a given agent.
    fn id(&self) -> &'static str;

    /// Human-readable location of the store (for CLI output).
    fn location(&self) -> String;

    /// Emit every tool call in the store, one per native call id, to `sink`.
    /// Unreadable inputs are skipped and counted, not fatal; an error from
    /// `sink` aborts the pass.
    fn visit(
        &self,
        sink: &mut dyn FnMut(RawToolCall) -> Result<(), HistoryError>,
    ) -> Result<SourceStats, HistoryError>;
}

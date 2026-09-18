//! Source adapters: one module per agent whose tool calls we ingest.
//!
//! An adapter turns an agent's native store (logs, a session DB, JSONL
//! transcripts) into [`RawToolCall`]s — exactly one per native call id —
//! and, for the cross-session memory, the sessions and human prompts around
//! them ([`SourceEvent`]). Nothing else: redaction, repository attribution
//! and storage are shared (see [`super::HistoryDb::ingest_source`] and
//! [`super::ingest`]). Adding an agent means adding a module here that
//! implements [`ToolCallSource`] and [`EventSource`].

pub mod claude_code;
pub mod jfc;
pub mod opencode;

use std::cell::Cell;
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

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
    /// Changed input units left for a later run (budget spent).
    pub deferred: usize,
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

/// A session starting (or being resumed): emitted before its prompts and calls.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawSession {
    /// The agent's id for the session; only its hash is stored.
    pub native_id: String,
    /// Native id of the session that spawned this one (a sub-agent).
    pub parent: Option<String>,
    /// Working directory the session ran in.
    pub cwd: Option<String>,
    pub started_ms: Option<i64>,
}

/// A human prompt: it starts a new episode of its session. Its text is never
/// read past what's needed to tell a human prompt from an injected one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawPrompt {
    /// The agent's id for the prompt message; only its hash is stored.
    pub native_id: String,
    /// Native id of the (root) session it was typed into.
    pub session: String,
    pub ts_ms: Option<i64>,
}

/// What an [`EventSource`] emits, in order per input unit: the unit's
/// session, its prompts and calls, then a checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceEvent {
    Session(RawSession),
    Prompt(RawPrompt),
    Call(Box<RawToolCall>),
    /// Everything of input unit `key` up to `value` has been emitted. The
    /// ingest stores it with the unit's events, atomically; the next pass
    /// receives it in [`Visit::cursors`] and resumes after it.
    Checkpoint {
        key: String,
        value: String,
    },
}

impl SourceEvent {
    /// A tool call event.
    pub fn call(raw: RawToolCall) -> Self {
        Self::Call(Box::new(raw))
    }
}

/// Per-run work limit: wall-clock time and events. Checked by adapters
/// between input units, so a run overshoots by at most one unit.
#[derive(Debug)]
pub struct Budget {
    deadline: Option<Instant>,
    max_events: Option<usize>,
    events: Cell<usize>,
}

impl Budget {
    /// No limit (a manual full ingest).
    pub fn unlimited() -> Self {
        Self::new(None, None)
    }

    pub fn new(time: Option<Duration>, max_events: Option<usize>) -> Self {
        Self {
            deadline: time.map(|t| Instant::now() + t),
            max_events,
            events: Cell::new(0),
        }
    }

    /// Count one emitted event.
    pub fn spend(&self) {
        self.events.set(self.events.get() + 1);
    }

    pub fn events(&self) -> usize {
        self.events.get()
    }

    /// Time or events are used up: adapters stop before the next unit.
    pub fn exhausted(&self) -> bool {
        self.deadline.is_some_and(|d| Instant::now() >= d)
            || self.max_events.is_some_and(|m| self.events.get() >= m)
    }
}

/// What an incremental pass knows before it starts.
pub struct Visit<'a> {
    /// Checkpoints earlier passes committed, by unit key.
    pub cursors: &'a HashMap<String, String>,
    pub budget: &'a Budget,
    /// Only sessions that worked under this directory (a repository).
    pub scope: Option<&'a Path>,
}

impl Visit<'_> {
    /// Whether a session in `cwd` is in scope.
    pub fn in_scope(&self, cwd: Option<&str>) -> bool {
        match (self.scope, cwd) {
            (None, _) => true,
            (Some(scope), Some(cwd)) => Path::new(cwd).starts_with(scope),
            (Some(_), None) => false,
        }
    }
}

/// An agent store read incrementally for the cross-session memory.
pub trait EventSource {
    /// Same id as the agent's [`ToolCallSource::id`].
    fn id(&self) -> &'static str;

    /// Human-readable location of the store (for CLI output).
    fn location(&self) -> String;

    /// Emit the events of every input unit changed since its checkpoint in
    /// `visit.cursors`, each unit closed by a [`SourceEvent::Checkpoint`],
    /// stopping between units once `visit.budget` is exhausted.
    fn visit_events(
        &self,
        visit: &Visit<'_>,
        sink: &mut dyn FnMut(SourceEvent) -> Result<(), HistoryError>,
    ) -> Result<SourceStats, HistoryError>;
}

/// Human prompts vs. text the harness injected as a user turn (command
/// wrappers, system reminders, interruptions, continuations).
pub(crate) fn is_human_prompt(text: &str) -> bool {
    let t = text.trim_start();
    if t.is_empty() {
        return false;
    }
    const INJECTED: &[&str] = &[
        "<",
        "[SYSTEM",
        "[system",
        "[Request interrupted",
        "Caveat:",
        "[TODO",
        "Continue if you have next steps",
        "This session is being continued",
    ];
    !INJECTED.iter().any(|p| t.starts_with(p))
        && !t.contains("TODO CONTINUATION")
        && !t.contains("SYSTEM DIRECTIVE")
}

/// Files an `apply_patch` body edits (`*** Update File: path`).
pub(crate) fn patch_files(patch: &str) -> Vec<String> {
    patch
        .lines()
        .filter_map(|l| {
            ["*** Update File: ", "*** Add File: ", "*** Delete File: "]
                .iter()
                .find_map(|p| l.strip_prefix(p))
        })
        .map(|p| p.trim().to_owned())
        .filter(|p| !p.is_empty())
        .take(40)
        .collect()
}

/// Fill a call's fields from a structured tool input (Claude Code's
/// `tool_use.input`, opencode's `state.input`): the keys the agents use for
/// commands, files, searches and symbols. Anything else in the input
/// (contents, edits, prompts) is ignored.
pub(crate) fn fill_from_input(raw: &mut RawToolCall, input: &serde_json::Value) {
    use serde_json::Value;
    let Some(obj) = input.as_object() else {
        return;
    };
    let text = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| obj.get(*k).and_then(Value::as_str))
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned)
    };
    raw.command = text(&["command", "cmd", "tmux_command"]);
    raw.file_path = text(&["file_path", "filePath", "notebook_path", "file"]);
    raw.pattern = text(&["pattern"]);
    raw.search_path = text(&["path"]);
    raw.query = text(&["query"]);
    if raw.file_path.is_none() {
        let lower = raw.tool.to_ascii_lowercase();
        if matches!(lower.as_str(), "read" | "view" | "edit" | "write") {
            raw.file_path = raw.search_path.take();
        }
    }
    if let Some(patch) = text(&["patchText", "patch"]) {
        raw.extra_paths = patch_files(&patch);
    }
    if let Some(symbol) = text(&["symbol"]) {
        raw.symbols.push(symbol);
    }
    match obj.get("symbols") {
        Some(Value::Array(items)) => raw.symbols.extend(
            items
                .iter()
                .filter_map(Value::as_str)
                .take(20)
                .map(str::to_owned),
        ),
        Some(Value::String(s)) => raw.symbols.push(s.clone()),
        _ => {}
    }
    raw.line = ["offset", "line"]
        .iter()
        .find_map(|k| obj.get(*k).and_then(Value::as_u64))
        .and_then(|n| u32::try_from(n).ok());
    if let Some(dir) = text(&["workdir", "cwd", "projectPath"]) {
        raw.cwd = Some(dir);
    }
}

/// Cap an output excerpt kept for error extraction.
pub(crate) const EXCERPT_BYTES: usize = 64 * 1024;

pub(crate) fn excerpt(text: &str) -> String {
    let mut end = text.len().min(EXCERPT_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// Truncated SHA-256 (32 hex) of a string: a stable, non-reversible key.
pub(crate) fn key_hash(s: &str) -> String {
    super::event::call_key("", s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injected_user_turns_are_not_prompts() {
        assert!(is_human_prompt("fix the flaky test in sync.rs"));
        assert!(!is_human_prompt("<command-name>/clear</command-name>"));
        assert!(!is_human_prompt("  <system-reminder>x</system-reminder>"));
        assert!(!is_human_prompt("[Request interrupted by user]"));
        assert!(!is_human_prompt(""));
    }

    #[test]
    fn patch_files_are_listed() {
        let patch = "*** Begin Patch\n*** Update File: src/a.rs\n@@\n-x\n+y\n*** Add File: b.md\n+hi\n*** End Patch";
        assert_eq!(patch_files(patch), ["src/a.rs", "b.md"]);
    }

    #[test]
    fn budget_counts_events() {
        let b = Budget::new(None, Some(2));
        assert!(!b.exhausted());
        b.spend();
        b.spend();
        assert!(b.exhausted());
        assert!(!Budget::unlimited().exhausted());
    }
}

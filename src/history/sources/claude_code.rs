//! Claude Code adapter: tool calls from `~/.claude/projects/**/*.jsonl`.
//!
//! What the parser relies on (Claude Code 2026-07 … 09):
//!
//! * One transcript per session: `<project>/<session>.jsonl`. Sub-agents
//!   write their own transcripts under `<project>/<session>/subagents/…/
//!   agent-*.jsonl`; they roll up to `<session>` (their parent).
//!   `journal.jsonl` files are workflow journals, not transcripts.
//! * Each line is a JSON record. `assistant` records carry `tool_use`
//!   blocks (`id`, `name`, `input`); a later `user` record carries the
//!   matching `tool_result` (`tool_use_id`, `content`, `is_error`). A call
//!   is emitted when its result arrives — one per `tool_use` id.
//! * Every record has the session's `cwd` and a `timestamp`.
//! * A human prompt is a root-transcript `user` record whose content is
//!   text (not a tool result), not `isMeta`/`isSidechain`/a compact
//!   summary, and not a harness wrapper (`<command-name>`, reminders, …).
//!   Only its uuid is used.
//!
//! Incremental: a transcript is checkpointed at the byte offset of the
//! first line not yet fully consumed — the earliest `tool_use` still
//! waiting for its result, else the end of the last complete line. A
//! transcript untouched for [`SETTLED`] emits its unanswered calls (an
//! interrupted session) and moves past them.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use serde::Deserialize;
use serde_json::Value;

use super::{
    EventSource,
    RawPrompt,
    RawSession,
    SourceEvent,
    SourceStats,
    Visit,
    excerpt,
    fill_from_input,
    is_human_prompt,
    key_hash,
};
use crate::history::activity::keeps_excerpt;
use crate::history::event::{CallResult, RawToolCall};
use crate::history::store::HistoryError;
use crate::history::time::{format_rfc3339_ms, parse_rfc3339_ms};

/// A transcript not written for this long is finished: calls still without
/// a result never get one.
const SETTLED: Duration = Duration::from_secs(10 * 60);

/// Claude Code's projects directory as an [`EventSource`].
#[derive(Debug, Clone)]
pub struct ClaudeCodeProjects {
    dir: PathBuf,
}

/// One transcript file.
#[derive(Debug, Clone)]
struct Transcript {
    path: PathBuf,
    /// Path under the projects dir (the checkpoint key is its hash).
    rel: String,
    /// `<project>/<root session>`: the family it rolls up to.
    family: String,
    native_id: String,
    parent: Option<String>,
    size: u64,
    modified: SystemTime,
}

impl ClaudeCodeProjects {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Transcripts grouped by root session, most recently written family
    /// first; within a family the root transcript precedes its sub-agents
    /// (their calls land in the root's episodes).
    fn transcripts(&self) -> Vec<Transcript> {
        let mut out = Vec::new();
        for entry in walkdir::WalkDir::new(&self.dir)
            .min_depth(2)
            .max_depth(6)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
        {
            let name = entry.file_name().to_string_lossy();
            if !entry.file_type().is_file() || !name.ends_with(".jsonl") || name == "journal.jsonl"
            {
                continue;
            }
            let Ok(rel) = entry.path().strip_prefix(&self.dir) else {
                continue;
            };
            let parts: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            let stem = name.trim_end_matches(".jsonl").to_owned();
            let (family, native_id, parent) = match parts.len() {
                2 => (format!("{}/{stem}", parts[0]), stem.clone(), None),
                n if n > 2 => {
                    let root = parts[1].clone();
                    (
                        format!("{}/{root}", parts[0]),
                        format!("{root}/{stem}"),
                        Some(root),
                    )
                }
                _ => continue,
            };
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            out.push(Transcript {
                path: entry.path().to_path_buf(),
                rel: parts.join("/"),
                family,
                native_id,
                parent,
                size: meta.len(),
                modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            });
        }
        let mut newest: HashMap<String, SystemTime> = HashMap::new();
        for t in &out {
            let e = newest.entry(t.family.clone()).or_insert(t.modified);
            *e = (*e).max(t.modified);
        }
        out.sort_by(|a, b| {
            newest[&b.family]
                .cmp(&newest[&a.family])
                .then_with(|| a.family.cmp(&b.family))
                .then_with(|| a.parent.is_some().cmp(&b.parent.is_some()))
                .then_with(|| a.rel.cmp(&b.rel))
        });
        out
    }
}

impl EventSource for ClaudeCodeProjects {
    fn id(&self) -> &'static str {
        "claude-code"
    }

    fn location(&self) -> String {
        self.dir.display().to_string()
    }

    fn visit_events(
        &self,
        visit: &Visit<'_>,
        sink: &mut dyn FnMut(SourceEvent) -> Result<(), HistoryError>,
    ) -> Result<SourceStats, HistoryError> {
        let mut stats = SourceStats::default();
        for t in self.transcripts() {
            let key = key_hash(&format!("cc:{}", t.rel));
            let mut from: u64 = visit
                .cursors
                .get(&key)
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            if from > t.size {
                from = 0; // rewritten or truncated
            }
            if from == t.size {
                continue;
            }
            if visit.budget.exhausted() {
                stats.deferred += 1;
                continue;
            }
            let settled = t.modified.elapsed().is_ok_and(|age| age >= SETTLED);
            match read_transcript(&t, from, settled, visit, sink, &mut stats) {
                Ok(Some(next)) => {
                    stats.inputs += 1;
                    sink(SourceEvent::Checkpoint {
                        key,
                        value: next.to_string(),
                    })?;
                }
                Ok(None) => {} // out of scope: leave it for an unscoped run
                Err(ReadError::Sink(e)) => return Err(e),
                Err(ReadError::Io) => stats.skipped += 1,
            }
        }
        Ok(stats)
    }
}

enum ReadError {
    Io,
    Sink(HistoryError),
}

impl From<HistoryError> for ReadError {
    fn from(e: HistoryError) -> Self {
        Self::Sink(e)
    }
}

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    cwd: Option<String>,
    timestamp: Option<String>,
    uuid: Option<String>,
    #[serde(default, rename = "isSidechain")]
    is_sidechain: bool,
    #[serde(default, rename = "isMeta")]
    is_meta: bool,
    #[serde(default, rename = "isCompactSummary")]
    is_compact_summary: bool,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    content: Option<Content>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Content {
    Text(String),
    Blocks(Vec<Block>),
}

#[derive(Deserialize)]
struct Block {
    #[serde(rename = "type")]
    kind: Option<String>,
    id: Option<String>,
    name: Option<String>,
    input: Option<Value>,
    tool_use_id: Option<String>,
    content: Option<Value>,
    is_error: Option<bool>,
    text: Option<String>,
}

/// A `tool_use` waiting for its result.
struct Pending {
    line_start: u64,
    raw: RawToolCall,
}

/// Emit the events of `t` from byte `from`; the next checkpoint, or `None`
/// when the transcript is out of the visit's scope.
fn read_transcript(
    t: &Transcript,
    from: u64,
    settled: bool,
    visit: &Visit<'_>,
    sink: &mut dyn FnMut(SourceEvent) -> Result<(), HistoryError>,
    stats: &mut SourceStats,
) -> Result<Option<u64>, ReadError> {
    let mut file = File::open(&t.path).map_err(|_| ReadError::Io)?;
    file.seek(SeekFrom::Start(from))
        .map_err(|_| ReadError::Io)?;
    let mut reader = BufReader::new(file);
    let mut pos = from;
    let mut buf = Vec::new();
    let mut pending: HashMap<String, Pending> = HashMap::new();
    let mut announced = false;
    loop {
        buf.clear();
        let n = reader
            .read_until(b'\n', &mut buf)
            .map_err(|_| ReadError::Io)?;
        if n == 0 || buf.last() != Some(&b'\n') {
            break; // EOF, or a line still being written
        }
        let line_start = pos;
        pos += n as u64;
        let text = String::from_utf8_lossy(&buf);
        if !(text.contains("\"type\":\"user\"") || text.contains("\"type\":\"assistant\"")) {
            continue;
        }
        let Ok(line) = serde_json::from_str::<Line>(&text) else {
            continue;
        };
        if !announced {
            let Some(cwd) = line.cwd.clone() else {
                continue;
            };
            if !visit.in_scope(Some(&cwd)) {
                return Ok(None);
            }
            sink(SourceEvent::Session(RawSession {
                native_id: t.native_id.clone(),
                parent: t.parent.clone(),
                cwd: Some(cwd),
                started_ms: line.timestamp.as_deref().and_then(parse_rfc3339_ms),
            }))?;
            announced = true;
        }
        let ts_ms = line.timestamp.as_deref().and_then(parse_rfc3339_ms);
        let injected = line.is_meta || line.is_sidechain || line.is_compact_summary;
        let content = line.message.and_then(|m| m.content);
        match line.kind.as_deref() {
            Some("assistant") => {
                let Some(Content::Blocks(blocks)) = content else {
                    continue;
                };
                for block in blocks {
                    if block.kind.as_deref() != Some("tool_use") {
                        continue;
                    }
                    let (Some(id), Some(name)) = (block.id, block.name) else {
                        continue;
                    };
                    let mut raw = RawToolCall {
                        native_id: id.clone(),
                        ts: ts_ms.map(format_rfc3339_ms),
                        ts_ms,
                        session: Some(t.native_id.clone()),
                        tool: name,
                        cwd: line.cwd.clone(),
                        ..Default::default()
                    };
                    if let Some(input) = &block.input {
                        fill_from_input(&mut raw, input);
                    }
                    pending.insert(id, Pending { line_start, raw });
                }
            }
            Some("user") => match content {
                Some(Content::Blocks(blocks)) => {
                    let mut text = String::new();
                    let mut had_result = false;
                    for block in blocks {
                        match block.kind.as_deref() {
                            Some("tool_result") => {
                                had_result = true;
                                let Some(p) = block.tool_use_id.and_then(|id| pending.remove(&id))
                                else {
                                    continue;
                                };
                                let raw =
                                    with_result(p.raw, block.content.as_ref(), block.is_error);
                                stats.calls += 1;
                                sink(SourceEvent::call(raw))?;
                            }
                            Some("text") if text.len() < 256 => {
                                text.push_str(block.text.as_deref().unwrap_or(""));
                            }
                            _ => {}
                        }
                    }
                    if !had_result {
                        prompt(t, line.uuid.as_deref(), injected, &text, ts_ms, sink)?;
                    }
                }
                Some(Content::Text(text)) => {
                    prompt(t, line.uuid.as_deref(), injected, &text, ts_ms, sink)?;
                }
                None => {}
            },
            _ => {}
        }
    }
    if pending.is_empty() || settled {
        let mut rest: Vec<Pending> = pending.into_values().collect();
        rest.sort_by_key(|p| p.line_start);
        for p in rest {
            stats.calls += 1;
            sink(SourceEvent::call(p.raw))?;
        }
        return Ok(Some(pos));
    }
    Ok(Some(
        pending.values().map(|p| p.line_start).min().unwrap_or(pos),
    ))
}

/// Emit a `user` record's prompt when a human typed it.
fn prompt(
    t: &Transcript,
    uuid: Option<&str>,
    injected: bool,
    text: &str,
    ts_ms: Option<i64>,
    sink: &mut dyn FnMut(SourceEvent) -> Result<(), HistoryError>,
) -> Result<(), HistoryError> {
    if t.parent.is_some() || injected || !is_human_prompt(text) {
        return Ok(());
    }
    let Some(uuid) = uuid else {
        return Ok(());
    };
    sink(SourceEvent::Prompt(RawPrompt {
        native_id: uuid.to_owned(),
        session: t.native_id.clone(),
        ts_ms,
    }))
}

/// Attach a `tool_result` to its call: size, error flag, and — for a
/// build/test command — a bounded head of the output (memory only).
fn with_result(
    mut raw: RawToolCall,
    content: Option<&Value>,
    is_error: Option<bool>,
) -> RawToolCall {
    let mut bytes = 0u64;
    let mut head = String::new();
    let wants_text = raw.command.as_deref().is_some_and(keeps_excerpt);
    let mut take = |s: &str| {
        bytes += s.len() as u64;
        if wants_text && head.len() < super::EXCERPT_BYTES {
            head.push_str(&excerpt(s));
            head.push('\n');
        }
    };
    match content {
        Some(Value::String(s)) => take(s),
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(s) = item.get("text").and_then(Value::as_str) {
                    take(s);
                }
            }
        }
        _ => {}
    }
    raw.result = Some(CallResult {
        is_error,
        exit_code: None,
        output_bytes: bytes,
        excerpt: (!head.is_empty()).then(|| excerpt(&head)),
    });
    raw
}

/// Default Claude Code projects directory: `~/.claude/projects`.
pub fn default_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("projects")
}

#[cfg(test)]
mod tests;

//! opencode adapter: tool calls from `~/.local/share/opencode/opencode.db`.
//!
//! The database is opened strictly read-only (`mode=ro`, never migrated,
//! never locked for writing). What the adapter relies on (opencode 2026-02 …
//! 09):
//!
//! * `session(id, parent_id, directory, time_created, time_updated)`: a
//!   sub-agent session names its parent; `directory` is its working dir.
//! * `message(id, session_id, time_created, data)`: `data.role` is `user`
//!   or `assistant`. A human prompt is a root-session user message with a
//!   non-synthetic text part that isn't a harness injection.
//! * `part(id, message_id, session_id, time_created, data)`: a tool call is
//!   a part whose `data` starts `{"type":"tool"` with `tool`, `callID` and
//!   `state {status, input, output, metadata.exit, error}`. Parts are read
//!   through the `(session_id, time_created)` index only — the table is
//!   tens of GB.
//!
//! Incremental: one unit per session, checkpointed as
//! `<time_updated>:<resume_from>` — a session whose `time_updated` hasn't
//! moved is skipped; one that has is re-read from `resume_from` (the
//! earliest call still running last time, else just past the last part).
//! Families (a root and its sub-agents) are read root first, most recently
//! updated family first.

use std::collections::HashMap;
use std::path::PathBuf;

use rusqlite::{Connection, OpenFlags, params};
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
use crate::history::time::format_rfc3339_ms;

/// opencode's session database as an [`EventSource`].
#[derive(Debug, Clone)]
pub struct OpencodeDb {
    path: PathBuf,
}

#[derive(Debug, Clone)]
struct Session {
    id: String,
    parent: Option<String>,
    directory: String,
    created: i64,
    updated: i64,
}

impl OpencodeDb {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn open(&self) -> rusqlite::Result<Connection> {
        let uri = format!("file:{}?mode=ro", self.path.display());
        let conn = Connection::open_with_flags(
            uri,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(conn)
    }
}

impl EventSource for OpencodeDb {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn location(&self) -> String {
        self.path.display().to_string()
    }

    fn visit_events(
        &self,
        visit: &Visit<'_>,
        sink: &mut dyn FnMut(SourceEvent) -> Result<(), HistoryError>,
    ) -> Result<SourceStats, HistoryError> {
        let mut stats = SourceStats::default();
        if !self.path.is_file() {
            return Ok(stats);
        }
        let conn = self.open()?;
        let sessions = load_sessions(&conn)?;
        let by_id: HashMap<&str, &Session> = sessions.iter().map(|s| (s.id.as_str(), s)).collect();
        let root_of = |s: &Session| -> String {
            let mut cur = s;
            for _ in 0..32 {
                match cur.parent.as_deref().and_then(|p| by_id.get(p)) {
                    Some(parent) => cur = parent,
                    None => break,
                }
            }
            cur.id.clone()
        };
        // Changed sessions, grouped by family.
        let mut families: HashMap<String, Vec<&Session>> = HashMap::new();
        for s in &sessions {
            let key = key_hash(&format!("oc:{}", s.id));
            let seen = visit.cursors.get(&key).and_then(|v| v.split_once(':'));
            if seen.is_some_and(|(updated, _)| updated == s.updated.to_string()) {
                continue;
            }
            families.entry(root_of(s)).or_default().push(s);
        }
        let mut order: Vec<(String, Vec<&Session>)> = families.into_iter().collect();
        order
            .sort_by_key(|(_, members)| std::cmp::Reverse(members.iter().map(|s| s.updated).max()));
        for (root, mut members) in order {
            let root_dir = by_id.get(root.as_str()).map(|s| s.directory.as_str());
            if !visit.in_scope(root_dir) {
                continue;
            }
            if visit.budget.exhausted() {
                stats.deferred += members.len();
                continue;
            }
            members.sort_by_key(|s| (s.parent.is_some(), s.created));
            for s in members {
                let key = key_hash(&format!("oc:{}", s.id));
                let from = visit
                    .cursors
                    .get(&key)
                    .and_then(|v| v.split_once(':'))
                    .and_then(|(_, from)| from.parse::<i64>().ok())
                    .unwrap_or(0);
                let next = read_session(&conn, s, from, sink, &mut stats)?;
                stats.inputs += 1;
                sink(SourceEvent::Checkpoint {
                    key,
                    value: format!("{}:{next}", s.updated),
                })?;
            }
        }
        Ok(stats)
    }
}

fn load_sessions(conn: &Connection) -> rusqlite::Result<Vec<Session>> {
    let mut stmt =
        conn.prepare("SELECT id, parent_id, directory, time_created, time_updated FROM session")?;
    let rows = stmt.query_map([], |r| {
        Ok(Session {
            id: r.get(0)?,
            parent: r.get(1)?,
            directory: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            created: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
            updated: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
        })
    })?;
    rows.collect()
}

/// Emit one session's prompts and calls from `from` (ms); the next
/// `resume_from`.
fn read_session(
    conn: &Connection,
    s: &Session,
    from: i64,
    sink: &mut dyn FnMut(SourceEvent) -> Result<(), HistoryError>,
    stats: &mut SourceStats,
) -> Result<i64, HistoryError> {
    sink(SourceEvent::Session(RawSession {
        native_id: s.id.clone(),
        parent: s.parent.clone(),
        cwd: Some(s.directory.clone()),
        started_ms: Some(s.created),
    }))?;
    if s.parent.is_none() {
        emit_prompts(conn, s, from, sink)?;
    }
    let mut stmt = conn.prepare_cached(
        "SELECT id, time_created,
                json_extract(data, '$.tool'),
                json_extract(data, '$.callID'),
                json_extract(data, '$.state.status'),
                json_extract(data, '$.state.input'),
                length(json_extract(data, '$.state.output')),
                json_extract(data, '$.state.metadata.exit'),
                CASE WHEN json_extract(data, '$.tool') IN ('bash', 'interactive_bash')
                     THEN substr(json_extract(data, '$.state.output'), 1, ?3) END
         FROM part
         WHERE session_id = ?1 AND time_created >= ?2
           AND substr(data, 1, 14) = '{\"type\":\"tool\"'
         ORDER BY time_created, id",
    )?;
    let mut rows = stmt.query(params![s.id, from, super::EXCERPT_BYTES as i64])?;
    let mut next = from;
    let mut running: Option<i64> = None;
    while let Some(r) = rows.next()? {
        let part_id: String = r.get(0)?;
        let created: i64 = r.get(1)?;
        next = next.max(created + 1);
        let Some(tool) = r.get::<_, Option<String>>(2)? else {
            continue;
        };
        let status: Option<String> = r.get(4)?;
        if matches!(status.as_deref(), Some("pending" | "running")) {
            running = Some(running.map_or(created, |t| t.min(created)));
            continue;
        }
        let mut raw = RawToolCall {
            native_id: r.get::<_, Option<String>>(3)?.unwrap_or(part_id),
            ts: Some(format_rfc3339_ms(created)),
            ts_ms: Some(created),
            session: Some(s.id.clone()),
            tool,
            cwd: Some(s.directory.clone()),
            ..Default::default()
        };
        if let Some(input) = r.get::<_, Option<String>>(5)? {
            if let Ok(value) = serde_json::from_str::<Value>(&input) {
                fill_from_input(&mut raw, &value);
            }
        }
        let output_bytes = r.get::<_, Option<i64>>(6)?.unwrap_or(0).max(0) as u64;
        let exit_code = r.get::<_, Option<i64>>(7).ok().flatten();
        let head: Option<String> = r.get(8).ok().flatten();
        let wants_text = raw.command.as_deref().is_some_and(keeps_excerpt);
        raw.result = Some(CallResult {
            is_error: status.as_deref().map(|s| s == "error"),
            exit_code,
            output_bytes,
            excerpt: head.filter(|_| wants_text).map(|h| excerpt(&h)),
        });
        stats.calls += 1;
        sink(SourceEvent::call(raw))?;
    }
    Ok(running.unwrap_or(next))
}

fn emit_prompts(
    conn: &Connection,
    s: &Session,
    from: i64,
    sink: &mut dyn FnMut(SourceEvent) -> Result<(), HistoryError>,
) -> Result<(), HistoryError> {
    let mut messages = conn.prepare_cached(
        "SELECT id, time_created FROM message
         WHERE session_id = ?1 AND time_created >= ?2
           AND json_extract(data, '$.role') = 'user'
         ORDER BY time_created, id",
    )?;
    let mut parts = conn.prepare_cached(
        "SELECT json_extract(data, '$.synthetic'), substr(json_extract(data, '$.text'), 1, 200)
         FROM part WHERE message_id = ?1 AND json_extract(data, '$.type') = 'text'",
    )?;
    let users: Vec<(String, i64)> = messages
        .query_map(params![s.id, from], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, created) in users {
        let texts: Vec<(Option<bool>, Option<String>)> = parts
            .query_map(params![id], |r| Ok((r.get(0).ok().flatten(), r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let human = texts.iter().any(|(synthetic, text)| {
            *synthetic != Some(true) && text.as_deref().is_some_and(is_human_prompt)
        });
        if human {
            sink(SourceEvent::Prompt(RawPrompt {
                native_id: id,
                session: s.id.clone(),
                ts_ms: Some(created),
            }))?;
        }
    }
    Ok(())
}

/// Default opencode database: `~/.local/share/opencode/opencode.db`.
pub fn default_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local/share/opencode/opencode.db")
}

#[cfg(test)]
mod tests;

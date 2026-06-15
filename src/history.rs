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
//! * **command profile** — the grep/cargo/git chains the user actually runs;
//!
//! and it is the corpus the vulnerability engine's fix-history learning
//! ([`codegraph_analysis::vuln::fix_history`]) draws on.
//!
//! Redaction happens at *parse* time ([`redact`]): credential-shaped values are
//! masked before a row is ever constructed, so no raw secret reaches disk.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};

const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS tool_events (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    ts           TEXT,
    session      TEXT,
    project      TEXT,
    tool_kind    TEXT NOT NULL,
    primary_cmd  TEXT,
    chain        TEXT,
    path         TEXT,
    redacted     INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_tool_events_project ON tool_events(project);
CREATE INDEX IF NOT EXISTS idx_tool_events_kind    ON tool_events(tool_kind);
CREATE INDEX IF NOT EXISTS idx_tool_events_session ON tool_events(session);
CREATE INDEX IF NOT EXISTS idx_tool_events_path    ON tool_events(path);
CREATE INDEX IF NOT EXISTS idx_tool_events_sess_path ON tool_events(session, path);
";

/// One parsed, redaction-clean tool invocation.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ToolEvent {
    pub ts: Option<String>,
    pub session: Option<String>,
    pub project: Option<String>,
    pub tool_kind: String,
    /// First command word for Bash events (e.g. `grep`, `cargo`).
    pub primary_cmd: Option<String>,
    /// Normalized command chain (e.g. `cd | cargo | grep`).
    pub chain: Option<String>,
    /// File path the tool touched (Read/Edit/Write), when present.
    pub path: Option<String>,
    /// Whether a credential-shaped value was masked while parsing.
    pub redacted: bool,
}

/// The default on-disk location: `~/.codegraph/history.db`.
pub fn default_history_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".codegraph").join("history.db")
}

/// Default JFC log directory to ingest from: `~/.config/jfc/logs`.
pub fn default_jfc_logs_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config").join("jfc").join("logs")
}

/// SQLite-backed history store.
pub struct HistoryDb {
    conn: Connection,
}

impl HistoryDb {
    /// Open (creating the parent dir + tables if needed).
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// In-memory store (tests).
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// Insert events in one transaction. Returns the number inserted.
    pub fn ingest(&mut self, events: &[ToolEvent]) -> rusqlite::Result<usize> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO tool_events \
                 (ts, session, project, tool_kind, primary_cmd, chain, path, redacted) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for e in events {
                stmt.execute(params![
                    e.ts,
                    e.session,
                    e.project,
                    e.tool_kind,
                    e.primary_cmd,
                    e.chain,
                    e.path,
                    e.redacted as i64,
                ])?;
            }
        }
        tx.commit()?;
        Ok(events.len())
    }

    /// Total rows.
    pub fn count(&self) -> rusqlite::Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM tool_events", [], |r| r.get(0))
    }

    /// Tool kinds by frequency.
    pub fn hot_tools(&self, limit: usize) -> rusqlite::Result<Vec<(String, i64)>> {
        self.grouped(
            "SELECT tool_kind, COUNT(*) c FROM tool_events \
             GROUP BY tool_kind ORDER BY c DESC, tool_kind LIMIT ?1",
            limit,
        )
    }

    /// Most-touched file paths (optionally scoped to a project substring).
    pub fn hot_files(
        &self,
        project: Option<&str>,
        limit: usize,
    ) -> rusqlite::Result<Vec<(String, i64)>> {
        match project {
            Some(p) => self.grouped_param(
                "SELECT path, COUNT(*) c FROM tool_events \
                 WHERE path IS NOT NULL AND project LIKE ?2 \
                 GROUP BY path ORDER BY c DESC, path LIMIT ?1",
                limit,
                &format!("%{p}%"),
            ),
            None => self.grouped(
                "SELECT path, COUNT(*) c FROM tool_events \
                 WHERE path IS NOT NULL GROUP BY path ORDER BY c DESC, path LIMIT ?1",
                limit,
            ),
        }
    }

    /// Most common normalized command chains.
    pub fn hot_chains(&self, limit: usize) -> rusqlite::Result<Vec<(String, i64)>> {
        self.grouped(
            "SELECT chain, COUNT(*) c FROM tool_events \
             WHERE chain IS NOT NULL AND chain <> '' \
             GROUP BY chain ORDER BY c DESC, chain LIMIT ?1",
            limit,
        )
    }

    /// File pairs read/edited in the same session — co-access coupling the
    /// static call graph misses. Requires at least 2 co-occurrences.
    ///
    /// Bounded for scale: a naive all-pairs self-join explodes on large
    /// sessions (quadratic in session size). We first reduce to the globally hottest
    /// files and the *distinct* (session, path) pairs among them, so the join
    /// is over a small, fixed candidate set regardless of corpus size.
    pub fn co_access(&self, limit: usize) -> rusqlite::Result<Vec<(String, String, i64)>> {
        const HOT_POOL: usize = 300;
        let mut stmt = self.conn.prepare(
            "WITH hot AS ( \
                 SELECT path FROM tool_events \
                 WHERE path IS NOT NULL \
                 GROUP BY path ORDER BY COUNT(*) DESC, path LIMIT ?2 \
             ), ev AS ( \
                 SELECT DISTINCT session, path FROM tool_events \
                 WHERE session IS NOT NULL AND path IS NOT NULL \
                   AND path IN (SELECT path FROM hot) \
             ) \
             SELECT a.path, b.path, COUNT(*) c \
             FROM ev a JOIN ev b ON a.session = b.session AND a.path < b.path \
             GROUP BY a.path, b.path HAVING c >= 2 \
             ORDER BY c DESC, a.path LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64, HOT_POOL as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        rows.collect()
    }

    fn grouped(&self, sql: &str, limit: usize) -> rusqlite::Result<Vec<(String, i64)>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        rows.collect()
    }

    fn grouped_param(
        &self,
        sql: &str,
        limit: usize,
        p: &str,
    ) -> rusqlite::Result<Vec<(String, i64)>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![limit as i64, p], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        rows.collect()
    }
}

// ─── Parsing & redaction ─────────────────────────────────────────────────────

/// Mask credential-shaped values. Returns `(masked, was_redacted)`.
///
/// Targets the shapes that actually appear in shell history: `--password X`,
/// `--token X`, `key=`/`secret=`/`password=`/`token=` assignments, `Bearer X`,
/// and JWT-looking `eyJ…` blobs. Deliberately conservative about flags so
/// benign `cargo -p crate` survives.
pub fn redact(input: &str) -> (String, bool) {
    let mut out = input.to_owned();
    let mut hit = false;

    // `--password VALUE` / `--token VALUE` (space- or =-separated).
    for flag in ["--password", "--token", "--secret", "--api-key", "--apikey"] {
        hit |= mask_after(&mut out, flag, true);
    }
    // `KEY=VALUE` credential assignments.
    for key in [
        "password=",
        "passwd=",
        "token=",
        "secret=",
        "api_key=",
        "apikey=",
        "access_token=",
        "PASSWORD=",
        "TOKEN=",
        "SECRET=",
        "API_KEY=",
    ] {
        hit |= mask_after(&mut out, key, false);
    }
    // `Bearer <blob>`.
    hit |= mask_after(&mut out, "Bearer ", false);
    // JWT-looking tokens anywhere.
    if let Some(masked) = mask_jwt(&out) {
        out = masked;
        hit = true;
    }
    (out, hit)
}

/// Replace the value following `marker` with `<REDACTED>`. When `flag` is true,
/// the value may be separated by a space or `=`; otherwise the marker already
/// ends with the separator. Operates on the first occurrence only (cheap and
/// sufficient — full lines rarely carry two distinct secrets).
fn mask_after(s: &mut String, marker: &str, flag: bool) -> bool {
    let Some(pos) = s.find(marker) else {
        return false;
    };
    let after = pos + marker.len();
    let bytes = s.as_bytes();
    let mut val_start = after;
    if flag {
        // Skip a single space or '=' separator.
        while val_start < bytes.len() && (bytes[val_start] == b' ' || bytes[val_start] == b'=') {
            val_start += 1;
        }
    }
    // Value ends at the next whitespace or quote.
    let mut val_end = val_start;
    while val_end < bytes.len()
        && !bytes[val_end].is_ascii_whitespace()
        && bytes[val_end] != b'"'
        && bytes[val_end] != b'\''
    {
        val_end += 1;
    }
    if val_end <= val_start {
        return false;
    }
    s.replace_range(val_start..val_end, "<REDACTED>");
    true
}

/// Mask a JWT-looking `eyJ…` blob if present.
fn mask_jwt(s: &str) -> Option<String> {
    let pos = s.find("eyJ")?;
    let bytes = s.as_bytes();
    let mut end = pos;
    let is_jwt = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || c == b'.';
    while end < bytes.len() && is_jwt(bytes[end]) {
        end += 1;
    }
    // Only treat it as a token if it's long enough to be one.
    if end - pos < 20 {
        return None;
    }
    let mut out = s.to_owned();
    out.replace_range(pos..end, "<REDACTED_JWT>");
    Some(out)
}

/// Extract the value of a `key=value` tracing field. Quoted values are
/// unquoted and may contain spaces; unquoted values run to the next space.
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let pos = line.find(key)?;
    let rest = &line[pos + key.len()..];
    if let Some(stripped) = rest.strip_prefix('"') {
        let end = stripped.find('"').unwrap_or(stripped.len());
        Some(&stripped[..end])
    } else {
        let end = rest.find(' ').unwrap_or(rest.len());
        Some(&rest[..end])
    }
}

/// The first command word of a shell command (skipping a leading
/// `VAR=val` env assignment), lowercased-as-written.
fn primary_of(cmd: &str) -> Option<String> {
    let first = cmd.split_whitespace().find(|tok| !tok.contains('='))?;
    Some(first.trim_start_matches("./").to_owned())
}

/// Normalize a command into a `seg | seg` chain of primary words across
/// `&&`/`||`/`|`/`;` separators (deduping consecutive repeats).
fn chain_of(cmd: &str) -> String {
    let mut segs: Vec<String> = Vec::new();
    for seg in cmd.split(['|', '&', ';']) {
        let seg = seg.trim();
        if seg.is_empty() {
            continue;
        }
        if let Some(p) = primary_of(seg) {
            if segs.last().map(String::as_str) != Some(p.as_str()) {
                segs.push(p);
            }
        }
    }
    segs.join(" | ")
}

/// Parse one log line into a [`ToolEvent`], or `None` if it isn't a tool record.
///
/// Handles the two record shapes JFC emits: a Bash exec (`cmd=…`) and a generic
/// tool dispatch (`kind=…`, including `kind=Mcp("…")`). `session`/`project` are
/// threaded by the caller (per-file context), not parsed here.
pub fn parse_log_line(line: &str) -> Option<ToolEvent> {
    let ts = line
        .split_whitespace()
        .next()
        .filter(|t| t.contains('T'))
        .map(str::to_owned);

    if let Some(raw_cmd) = field(line, "cmd=") {
        let (cmd, redacted) = redact(raw_cmd);
        return Some(ToolEvent {
            ts,
            tool_kind: "Bash".to_owned(),
            primary_cmd: primary_of(&cmd),
            chain: Some(chain_of(&cmd)),
            // cwd is the working *directory*, not a file target — record it as
            // the project context so `path` stays file-only (clean hot-files).
            project: field(line, "cwd=").map(str::to_owned),
            redacted,
            ..Default::default()
        });
    }

    if let Some(kind_raw) = field(line, "kind=") {
        // Strip tracing span punctuation (`kind=Read}:` → `Read`) and unwrap
        // `Mcp("name")` to `name`.
        let kind_trimmed = kind_raw.trim_matches(|c| matches!(c, '}' | ':' | ',' | '"' | ' '));
        let tool_kind = kind_trimmed
            .strip_prefix("Mcp(\"")
            .and_then(|s| s.strip_suffix("\")"))
            .unwrap_or(kind_trimmed)
            .to_owned();
        // Reject non-tool `kind=` fields: notify file-watcher events
        // (`Modify(Name(Both))`) and stream block kinds (`text`, `reasoning`).
        if tool_kind.is_empty()
            || tool_kind.contains('(')
            || matches!(
                tool_kind.as_str(),
                "text" | "reasoning" | "thinking" | "input_json" | "redacted_thinking"
            )
        {
            return None;
        }
        let path = field(line, "path=").map(str::to_owned);
        let (path, redacted) = match path {
            Some(p) => {
                let (r, hit) = redact(&p);
                (Some(r), hit)
            }
            None => (None, false),
        };
        return Some(ToolEvent {
            ts,
            tool_kind,
            path,
            redacted,
            ..Default::default()
        });
    }
    None
}

/// Parse every `*.log*` file under `dir`, tagging each event with the file's
/// stem as a session id. Best-effort: unreadable files are skipped.
pub fn parse_logs_dir(dir: &Path) -> Vec<ToolEvent> {
    let mut events = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return events;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !name.contains(".log") {
            continue;
        }
        let session = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned);
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in content.lines() {
            if let Some(mut ev) = parse_log_line(line) {
                ev.session = session.clone();
                events.push(ev);
            }
        }
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_password_flag_but_keeps_cargo_p() {
        let (out, hit) = redact("ssh host --password hunter2 && cargo -p mycrate build");
        assert!(hit);
        assert!(out.contains("--password <REDACTED>"));
        assert!(
            out.contains("cargo -p mycrate"),
            "benign -p must survive: {out}"
        );
    }

    #[test]
    fn redacts_token_assignment_and_jwt() {
        let (out, hit) = redact("export TOKEN=eyJhbGciOiJIUzI1NiwideadbeefdeadbeefXX.payload.sig");
        assert!(hit);
        assert!(out.contains("<REDACTED"), "got {out}");
        assert!(!out.contains("eyJhbGci"));
    }

    #[test]
    fn parses_bash_exec_line() {
        let line = r#"2026-05-21T05:51:54Z DEBUG jfc::tool: bash executing cmd="cd /repo && grep -n foo src" timeout_ms=120000 cwd=/repo"#;
        let ev = parse_log_line(line).expect("should parse");
        assert_eq!(ev.tool_kind, "Bash");
        assert_eq!(ev.primary_cmd.as_deref(), Some("cd"));
        assert_eq!(ev.chain.as_deref(), Some("cd | grep"));
        assert_eq!(ev.path, None, "cwd is project context, not a file path");
        assert_eq!(ev.project.as_deref(), Some("/repo"));
        assert_eq!(ev.ts.as_deref(), Some("2026-05-21T05:51:54Z"));
    }

    #[test]
    fn parses_mcp_kind_line() {
        let line = r#"2026-05-21T05:51:54Z INFO execute_tool{ kind=Mcp("mcp__codegraph__codegraph_explore") path=src/lib.rs }"#;
        let ev = parse_log_line(line).expect("should parse");
        assert_eq!(ev.tool_kind, "mcp__codegraph__codegraph_explore");
        assert_eq!(ev.path.as_deref(), Some("src/lib.rs"));
    }

    #[test]
    fn ignores_non_tool_lines() {
        assert!(
            parse_log_line("2026-05-21T05:51:54Z INFO tracing initialized log_dir=/x").is_none()
        );
    }

    #[test]
    fn strips_span_punctuation_and_rejects_noise_kinds() {
        // tracing span form `kind=Read}:` collapses to `Read`.
        let ev = parse_log_line("INFO execute_tool{ kind=Read}: serving").unwrap();
        assert_eq!(ev.tool_kind, "Read");
        // notify file-watcher events and stream block kinds are not tools.
        assert!(parse_log_line("DEBUG watcher kind=Modify(Name(Both)) path=x").is_none());
        assert!(parse_log_line("DEBUG stream kind=reasoning len=5").is_none());
    }

    #[test]
    fn ingest_and_aggregate() {
        let mut db = HistoryDb::open_in_memory().unwrap();
        let events = vec![
            ToolEvent {
                session: Some("s1".into()),
                project: Some("/repo".into()),
                tool_kind: "Read".into(),
                path: Some("a.rs".into()),
                ..Default::default()
            },
            ToolEvent {
                session: Some("s1".into()),
                project: Some("/repo".into()),
                tool_kind: "Read".into(),
                path: Some("b.rs".into()),
                ..Default::default()
            },
            ToolEvent {
                session: Some("s2".into()),
                project: Some("/repo".into()),
                tool_kind: "Read".into(),
                path: Some("a.rs".into()),
                ..Default::default()
            },
            ToolEvent {
                session: Some("s2".into()),
                project: Some("/repo".into()),
                tool_kind: "Read".into(),
                path: Some("b.rs".into()),
                ..Default::default()
            },
        ];
        assert_eq!(db.ingest(&events).unwrap(), 4);
        assert_eq!(db.count().unwrap(), 4);

        let hot = db.hot_files(None, 10).unwrap();
        assert_eq!(hot[0].1, 2); // a.rs and b.rs each seen twice

        // a.rs and b.rs co-occur in both sessions -> co-access count 2.
        let co = db.co_access(10).unwrap();
        assert_eq!(co.len(), 1);
        assert_eq!(co[0].2, 2);

        let tools = db.hot_tools(10).unwrap();
        assert_eq!(tools[0], ("Read".to_string(), 4));
    }
}

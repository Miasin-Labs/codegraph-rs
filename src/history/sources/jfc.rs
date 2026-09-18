//! JFC adapter: tool calls from JFC's `tracing` logs (`~/.config/jfc/logs`).
//!
//! What the parser relies on (JFC builds 2026-05 … 2026-08):
//!
//! * A record starts with an RFC 3339 timestamp. A multi-line field value (a
//!   Display-formatted shell command) continues on the following
//!   un-timestamped lines, with the record's later fields on its last line.
//! * Every call is announced with its native id, by one or more of
//!   `content_block_start tool_use … tool_name= tool_use_id=` (Anthropic
//!   provider only), `[synthesize] tool_done … tool_name= tool_use_id=`,
//!   the UI's `StreamTool received` / `route=…` / `approved → dispatch`
//!   (`tool_kind= tool_id=`), and the scheduler's `executing sequential
//!   tool` / `tool completed` (`tool_id= kind=`).
//!   Calls are keyed on that id, so however many lines mention a call it
//!   yields one [`RawToolCall`].
//! * Execution logs from inside `execute_tool*{… kind=K}` spans, and *every*
//!   line in such a span repeats `kind=` — the old per-line parser counted
//!   each as a call (4.6× over-count). Newer builds also put the id in the
//!   span (`runtime_tool_id=Some("…")`, `tool_id=Some("…")`); for older ones
//!   a detail pairs with the oldest call of its kind still waiting for one.
//! * Details: `bash: executing [task_id=] cmd=<Display> [timeout_ms= …
//!   cwd= output_path=]` and `read|edit|write: starting file_path=<Display> …`.
//!   Display values are unquoted and may contain spaces, so they run to the
//!   next field the log writes after them, not to the first space.
//! * Working directory: a Bash detail's own `cwd=`, else the session's
//!   latest `project_root=` / `session saved … cwd=`.
//! * JFC logs at most the first 100 characters of a command (58% of the
//!   2026-05 … 08 records are cut there), so a long command's chain is only
//!   its prefix — and a secret may be cut mid-value (the redactor fails
//!   closed on an unterminated quote).

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};

use super::{SourceStats, ToolCallSource};
use crate::history::event::RawToolCall;
use crate::history::store::HistoryError;

/// Cap on a buffered multi-line record (a command with a huge heredoc).
const MAX_RECORD: usize = 256 * 1024;

/// Fields JFC logs after a Bash `cmd=`, in the order it writes them.
const CMD_STOPS: &[&str] = &[
    " timeout_ms=",
    " track_for_abort=",
    " cwd=",
    " output_path=",
];

/// Fields JFC logs after a `file_path=`, in the order it writes them.
const FILE_PATH_STOPS: &[&str] = &[" offset=", " limit=", " old_len=", " content_len=", " cwd="];

/// JFC's log directory as a [`ToolCallSource`].
#[derive(Debug, Clone)]
pub struct JfcLogs {
    dir: PathBuf,
}

impl JfcLogs {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Regular files whose name contains `.log`, sorted. Symlinks are
    /// skipped: `latest.log` aliases a session file.
    fn log_files(&self) -> io::Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let is_log = entry.file_name().to_string_lossy().contains(".log");
            if is_log && entry.file_type()?.is_file() {
                files.push(entry.path());
            }
        }
        files.sort();
        Ok(files)
    }
}

impl ToolCallSource for JfcLogs {
    fn id(&self) -> &'static str {
        "jfc"
    }

    fn location(&self) -> String {
        self.dir.display().to_string()
    }

    fn visit(
        &self,
        sink: &mut dyn FnMut(RawToolCall) -> Result<(), HistoryError>,
    ) -> Result<SourceStats, HistoryError> {
        let files = match self.log_files() {
            Ok(files) => files,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(SourceStats::default()),
            Err(e) => return Err(e.into()),
        };
        let mut stats = SourceStats::default();
        for path in files {
            let parsed =
                File::open(&path).and_then(|f| parse_log(BufReader::new(f), &session_name(&path)));
            let Ok(calls) = parsed else {
                stats.skipped += 1;
                continue;
            };
            stats.inputs += 1;
            for call in calls {
                stats.calls += 1;
                sink(call)?;
            }
        }
        Ok(stats)
    }
}

/// Session id for a log file: its name without a trailing `.log`
/// (`ses_20260810_141304`, `jfc.log.2026-05-05`, `jfc-cli`).
fn session_name(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match name.strip_suffix(".log") {
        Some(stem) => stem.to_owned(),
        None => name,
    }
}

/// Parse one JFC log into its tool calls, in first-announced order.
/// Invalid UTF-8 is replaced, never fatal.
pub fn parse_log<R: BufRead>(mut reader: R, session: &str) -> io::Result<Vec<RawToolCall>> {
    let mut parser = LogParser::new(session);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        if reader.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        let line = String::from_utf8_lossy(&buf);
        parser.line(line.trim_end_matches(['\n', '\r']));
    }
    Ok(parser.finish())
}

/// How authoritative a tool name is: the UI's parsed `ToolKind` beats the
/// scheduler's, which beats the model-emitted name (`bash`, `graph_search`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum NameRank {
    Detail,
    ModelName,
    Scheduler,
    ToolKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailKind {
    Bash,
    Read,
    Edit,
    Write,
}

impl DetailKind {
    fn of_message(msg: &str) -> Option<Self> {
        [
            ("bash: executing", Self::Bash),
            ("read: starting", Self::Read),
            ("edit: starting", Self::Edit),
            ("write: starting", Self::Write),
        ]
        .into_iter()
        .find_map(|(prefix, kind)| msg.starts_with(prefix).then_some(kind))
    }

    fn tool(self) -> &'static str {
        match self {
            Self::Bash => "Bash",
            Self::Read => "Read",
            Self::Edit => "Edit",
            Self::Write => "Write",
        }
    }
}

/// The innermost `execute_tool*{…}` span of a detail line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Span {
    id: Option<String>,
    kind: Option<String>,
}

struct Call {
    raw: RawToolCall,
    rank: NameRank,
    /// A command or file detail has been attached.
    detailed: bool,
}

/// A detail record still collecting continuation lines.
struct Pending {
    kind: DetailKind,
    span: Span,
    ts: Option<String>,
    line_no: usize,
    text: String,
}

struct LogParser {
    session: String,
    calls: Vec<Call>,
    by_id: HashMap<String, usize>,
    /// Calls per canonical tool name, oldest first, for id-less details.
    awaiting: HashMap<String, VecDeque<usize>>,
    session_cwd: Option<String>,
    first_cwd: Option<String>,
    pending: Option<Pending>,
    line_no: usize,
}

impl LogParser {
    fn new(session: &str) -> Self {
        Self {
            session: session.to_owned(),
            calls: Vec::new(),
            by_id: HashMap::new(),
            awaiting: HashMap::new(),
            session_cwd: None,
            first_cwd: None,
            pending: None,
            line_no: 0,
        }
    }

    fn line(&mut self, line: &str) {
        self.line_no += 1;
        let Some(ts) = timestamp(line) else {
            if let Some(p) = self.pending.as_mut() {
                push_capped(&mut p.text, "\n");
                push_capped(&mut p.text, line);
            }
            return;
        };
        self.flush_pending();
        let Some((target_at, target, msg)) = split_target(line) else {
            return;
        };
        let ts = Some(ts.to_owned());
        if target == "jfc::tools" {
            self.detail(&line[..target_at], msg, ts);
        } else {
            self.announcement(target, msg, ts);
        }
    }

    fn announcement(&mut self, target: &str, msg: &str, ts: Option<String>) {
        let model_named = [
            "content_block_start tool_use",
            "tool_done ",
            "synthesize tool_done",
        ];
        let (id_key, name_key, rank) = if model_named.iter().any(|p| msg.starts_with(p)) {
            ("tool_use_id", "tool_name", NameRank::ModelName)
        } else if target.starts_with("jfc::ui") && msg.contains(" tool_kind=") {
            // `StreamTool received`, `route=… (no approval needed)`, `approved → dispatch`.
            ("tool_id", "tool_kind", NameRank::ToolKind)
        } else if target == "jfc::scheduler"
            && (msg.starts_with("executing sequential tool") || msg.starts_with("tool completed "))
        {
            ("tool_id", "kind", NameRank::Scheduler)
        } else {
            self.track_cwd(target, msg);
            return;
        };
        let id = field(msg, id_key);
        let name = field(msg, name_key).and_then(|n| canonical_tool(&n));
        if let (Some(id), Some(name)) = (id, name) {
            self.announce(&id, name, rank, ts);
        }
    }

    fn track_cwd(&mut self, target: &str, msg: &str) {
        let cwd = match target {
            "jfc::agents" | "jfc::context" => field(msg, "project_root"),
            "jfc::session" if msg.starts_with("session saved") => field(msg, "cwd"),
            _ => None,
        };
        if let Some(cwd) = cwd.filter(|c| c.starts_with('/') || c.starts_with('~')) {
            let cwd = cwd.into_owned();
            self.first_cwd.get_or_insert_with(|| cwd.clone());
            self.session_cwd = Some(cwd);
        }
    }

    fn announce(&mut self, id: &str, name: String, rank: NameRank, ts: Option<String>) {
        match self.by_id.get(id) {
            Some(&i) => {
                let call = &mut self.calls[i];
                if rank > call.rank {
                    call.raw.tool = name;
                    call.rank = rank;
                }
            }
            None => {
                self.register(id.to_owned(), name, rank, ts);
            }
        }
    }

    fn register(&mut self, id: String, tool: String, rank: NameRank, ts: Option<String>) -> usize {
        let i = self.calls.len();
        self.awaiting.entry(tool.clone()).or_default().push_back(i);
        self.by_id.insert(id.clone(), i);
        self.calls.push(Call {
            raw: RawToolCall {
                native_id: id,
                ts,
                session: Some(self.session.clone()),
                tool,
                cwd: self.session_cwd.clone(),
                ..Default::default()
            },
            rank,
            detailed: false,
        });
        i
    }

    fn detail(&mut self, spans: &str, msg: &str, ts: Option<String>) {
        let Some(kind) = DetailKind::of_message(msg) else {
            return;
        };
        let span = innermost_span(spans);
        if kind == DetailKind::Bash {
            // The command may continue on the next lines; finish it later.
            let mut text = String::new();
            push_capped(&mut text, msg);
            self.pending = Some(Pending {
                kind,
                span,
                ts,
                line_no: self.line_no,
                text,
            });
            return;
        }
        let path = display_field(msg, "file_path", FILE_PATH_STOPS).map(Cow::into_owned);
        let line_no = self.line_no;
        self.attach(kind, span, ts, line_no, |raw| {
            if raw.file_path.is_none() {
                raw.file_path = path;
            }
        });
    }

    fn flush_pending(&mut self) {
        let Some(p) = self.pending.take() else {
            return;
        };
        let (command, cwd) = bash_fields(&p.text);
        self.attach(p.kind, p.span, p.ts, p.line_no, |raw| {
            if raw.command.is_none() {
                raw.command = command;
                if cwd.is_some() {
                    raw.cwd = cwd;
                }
            }
        });
    }

    /// Attach a detail to its call: by the span's id when it carries one,
    /// else to the oldest call of the span's kind still waiting for a detail,
    /// else to a new call keyed by its line (a call no line announced).
    fn attach(
        &mut self,
        kind: DetailKind,
        span: Span,
        ts: Option<String>,
        line_no: usize,
        apply: impl FnOnce(&mut RawToolCall),
    ) {
        let tool = span
            .kind
            .as_deref()
            .and_then(canonical_tool)
            .unwrap_or_else(|| kind.tool().to_owned());
        let idx = match span.id {
            Some(id) => match self.by_id.get(&id) {
                Some(&i) => i,
                None => self.register(id, tool, NameRank::Detail, ts.clone()),
            },
            None => match self.next_awaiting(&tool) {
                Some(i) => i,
                None => {
                    let id = format!("{}#L{line_no}", self.session);
                    self.register(id, tool, NameRank::Detail, ts.clone())
                }
            },
        };
        let call = &mut self.calls[idx];
        call.detailed = true;
        if call.raw.ts.is_none() {
            call.raw.ts = ts;
        }
        apply(&mut call.raw);
    }

    fn next_awaiting(&mut self, tool: &str) -> Option<usize> {
        let queue = self.awaiting.get_mut(tool)?;
        while let Some(i) = queue.pop_front() {
            if !self.calls[i].detailed {
                return Some(i);
            }
        }
        None
    }

    fn finish(mut self) -> Vec<RawToolCall> {
        self.flush_pending();
        let first_cwd = self.first_cwd;
        self.calls
            .into_iter()
            .map(|c| {
                let mut raw = c.raw;
                if raw.cwd.is_none() {
                    raw.cwd.clone_from(&first_cwd);
                }
                raw
            })
            .collect()
    }
}

// ─── line & field syntax ─────────────────────────────────────────────────────

/// The leading RFC 3339 timestamp of a record's first line.
fn timestamp(line: &str) -> Option<&str> {
    let b = line.as_bytes();
    let shaped = b.len() >= 20
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T';
    if !shaped {
        return None;
    }
    line.split_ascii_whitespace().next()
}

/// `(offset of the target, target, message)` of a `jfc::` record:
/// `TS LEVEL spans: jfc::tools: message…`.
fn split_target(line: &str) -> Option<(usize, &str, &str)> {
    let at = line.find(" jfc::")? + 1;
    let rest = &line[at..];
    let end = rest.find(": ")?;
    Some((at, &rest[..end], &rest[end + 2..]))
}

/// Offset just past `key=`, where `key` starts a field (not the tail of a
/// longer name: `tool_id=` must not match inside `runtime_tool_id=`).
fn find_field(s: &str, key: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut from = 0;
    while let Some(pos) = s[from..].find(key) {
        let at = from + pos;
        let end = at + key.len();
        let starts_field =
            at == 0 || !(bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_');
        if starts_field && bytes.get(end) == Some(&b'=') {
            return Some(end + 1);
        }
        from = end;
    }
    None
}

/// A simple field: a Debug-quoted string, or the token up to whitespace, `,` or `}`.
fn field<'a>(s: &'a str, key: &str) -> Option<Cow<'a, str>> {
    let rest = &s[find_field(s, key)?..];
    if let Some(quoted) = rest.strip_prefix('"') {
        return Some(Cow::Owned(unquote(quoted).0));
    }
    let end = rest
        .find(|c: char| c.is_whitespace() || c == ',' || c == '}')
        .unwrap_or(rest.len());
    (end > 0).then(|| Cow::Borrowed(&rest[..end]))
}

/// A Display-formatted field that may contain spaces: it runs to the last
/// occurrence of the first of `stops` (the fields logged after it) present,
/// else to the end.
fn display_field<'a>(s: &'a str, key: &str, stops: &[&str]) -> Option<Cow<'a, str>> {
    let rest = &s[find_field(s, key)?..];
    if let Some(quoted) = rest.strip_prefix('"') {
        return Some(Cow::Owned(unquote(quoted).0));
    }
    let end = stops
        .iter()
        .find_map(|stop| rest.rfind(stop))
        .unwrap_or(rest.len());
    let value = rest[..end].trim_end();
    (!value.is_empty()).then_some(Cow::Borrowed(value))
}

/// `(command, cwd)` of a (possibly multi-line) `bash: executing` record.
fn bash_fields(text: &str) -> (Option<String>, Option<String>) {
    let Some(start) = find_field(text, "cmd") else {
        return (None, None);
    };
    let rest = &text[start..];
    let (command, tail) = match rest.strip_prefix('"') {
        Some(quoted) => {
            let (value, used) = unquote(quoted);
            (value, &quoted[used..])
        }
        None => {
            let stop = CMD_STOPS
                .iter()
                .find_map(|s| rest.rfind(s))
                .unwrap_or(rest.len());
            (rest[..stop].trim_end().to_owned(), &rest[stop..])
        }
    };
    let cwd = display_field(tail, "cwd", &[" output_path="]).map(Cow::into_owned);
    ((!command.is_empty()).then_some(command), cwd)
}

/// Unescape a Rust `Debug` string body (after its opening quote). Returns the
/// value and the bytes consumed, closing quote included.
fn unquote(s: &str) -> (String, usize) {
    let mut out = String::new();
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return (out, i + 1),
            '\\' => match chars.next() {
                Some((_, 'n')) => out.push('\n'),
                Some((_, 't')) => out.push('\t'),
                Some((_, 'r')) => out.push('\r'),
                Some((_, '0')) => out.push('\0'),
                Some((_, 'u')) => {
                    let mut hex = String::new();
                    for (_, h) in chars.by_ref() {
                        match h {
                            '{' => {}
                            '}' => break,
                            h => hex.push(h),
                        }
                    }
                    if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        out.push(ch);
                    }
                }
                Some((_, other)) => out.push(other),
                None => break,
            },
            c => out.push(c),
        }
    }
    (out, s.len())
}

/// The id and kind of the innermost `execute_tool*{…}` span in `spans`.
fn innermost_span(spans: &str) -> Span {
    let Some(at) = spans.rfind("execute_tool") else {
        return Span::default();
    };
    let rest = &spans[at..];
    let Some(open) = rest.find('{') else {
        return Span::default();
    };
    let body = &rest[open + 1..];
    let body = &body[..body.find('}').unwrap_or(body.len())];
    let id = field(body, "runtime_tool_id")
        .or_else(|| field(body, "tool_id"))
        .and_then(|v| span_id(&v));
    let kind = field(body, "kind").map(Cow::into_owned);
    Span { id, kind }
}

/// `Some("toolu_…")` → `toolu_…`; `None` → nothing; a bare id as is.
fn span_id(v: &str) -> Option<String> {
    if v == "None" {
        return None;
    }
    let v = match v.strip_prefix("Some(") {
        Some(inner) => inner.trim_end_matches(')').trim_matches('"'),
        None => v.trim_matches('"'),
    };
    (!v.is_empty()).then(|| v.to_owned())
}

fn push_capped(buf: &mut String, s: &str) {
    let room = MAX_RECORD.saturating_sub(buf.len());
    if room == 0 {
        return;
    }
    let mut end = s.len().min(room);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    buf.push_str(&s[..end]);
}

/// Lower-case JFC tool names mapped to the `ToolKind` spelling (models
/// sometimes emit `bash`, `taskupdate`, `graph_search`).
const KNOWN_TOOLS: &[&str] = &[
    "AskModel",
    "AskUserQuestion",
    "Bash",
    "BashOutput",
    "Edit",
    "EnterPlanMode",
    "ExitPlanMode",
    "Glob",
    "Grep",
    "KillShell",
    "MultiEdit",
    "NotebookEdit",
    "Read",
    "ScratchpadRead",
    "ScratchpadWrite",
    "SendUserMessage",
    "SetGoal",
    "StructuredOutput",
    "Task",
    "TaskCreate",
    "TaskDone",
    "TaskGet",
    "TaskList",
    "TaskUpdate",
    "TaskValidate",
    "TeamCreate",
    "TodoWrite",
    "ToolSearch",
    "WebFetch",
    "WebSearch",
    "Write",
];

/// Canonical tool name: `Mcp("x")` → `x`; `bash` → `Bash`; `graph_search` →
/// `GraphSearch`; `mcp__…` kept. `None` for an empty or `None` name.
fn canonical_tool(name: &str) -> Option<String> {
    let name = name.trim();
    let name = name
        .strip_prefix("Mcp(\"")
        .and_then(|s| s.strip_suffix("\")"))
        .unwrap_or(name);
    let end = name
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
        .unwrap_or(name.len());
    let name = &name[..end];
    if name.is_empty() || name == "None" {
        return None;
    }
    if name.starts_with("mcp__") {
        return Some(name.to_owned());
    }
    if let Some(known) = KNOWN_TOOLS.iter().find(|k| k.eq_ignore_ascii_case(name)) {
        return Some((*known).to_owned());
    }
    if name.contains('_') {
        return Some(
            name.split('_')
                .filter(|p| !p.is_empty())
                .map(capitalize)
                .collect(),
        );
    }
    if name.starts_with(|c: char| c.is_ascii_lowercase()) {
        return Some(capitalize(name));
    }
    Some(name.to_owned())
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    chars
        .next()
        .map(|c| c.to_ascii_uppercase().to_string() + chars.as_str())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;

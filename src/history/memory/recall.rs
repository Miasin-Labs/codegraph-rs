//! `recall`: what earlier sessions in a repository did — episodes that
//! touched a path, where an identifier was found, recent failures and
//! whether they were fixed, and files edited together.
//!
//! Every query is an indexed, `LIMIT`ed read; [`recall_at`] runs them on a
//! read-only connection under a hard deadline (SQLite interrupt) and bounds
//! the answer to [`RECALL_BUDGET`] bytes of JSON.

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use super::index_probe::IndexProbe;
use super::queries::Queries;
use super::report::{RECALL_BUDGET, RecallReport};
use crate::history::repo::RepoLocator;
use crate::history::schema::CURRENT_VERSION;
use crate::history::store::HistoryError;
use crate::history::time::now_ms;

/// What to recall.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum About {
    /// Episodes that touched files under a repo-relative path prefix.
    Path(String),
    /// Where an identifier was looked up and found.
    Symbol(String),
    /// Recent failing builds/tests, with whether a later run passed.
    Failures,
    /// Files edited together (sessions and git), optionally for one path.
    Cochange(Option<String>),
    /// The most recent episodes.
    Last,
}

impl About {
    /// `path` | `symbol:<name>` | `failures` | `cochange[:<path>]` | `last`.
    pub fn parse(s: &str) -> Self {
        let s = s.trim();
        match s {
            "" | "last" => Self::Last,
            "failures" | "failure" | "failed" => Self::Failures,
            "cochange" | "co-change" => Self::Cochange(None),
            _ => {
                if let Some(name) = s.strip_prefix("symbol:") {
                    Self::Symbol(name.trim().to_owned())
                } else if let Some(path) = s
                    .strip_prefix("cochange:")
                    .or_else(|| s.strip_prefix("co-change:"))
                {
                    Self::Cochange(Some(clean_path(path)))
                } else {
                    Self::Path(clean_path(s.strip_prefix("path:").unwrap_or(s)))
                }
            }
        }
    }

    pub(crate) fn label(&self) -> String {
        match self {
            Self::Path(p) => p.clone(),
            Self::Symbol(s) => format!("symbol:{s}"),
            Self::Failures => "failures".into(),
            Self::Cochange(None) => "cochange".into(),
            Self::Cochange(Some(p)) => format!("cochange:{p}"),
            Self::Last => "last".into(),
        }
    }

    /// Re-anchor a repo-relative path given relative to a sub-directory
    /// (a codegraph project nested in the repository).
    pub fn under(self, base: &str) -> Self {
        let join = |p: String| {
            if base.is_empty() || p.starts_with(base) {
                p
            } else {
                format!("{}/{p}", base.trim_end_matches('/'))
            }
        };
        match self {
            Self::Path(p) => Self::Path(join(p)),
            Self::Cochange(Some(p)) => Self::Cochange(Some(join(p))),
            other => other,
        }
    }
}

fn clean_path(p: &str) -> String {
    p.trim().trim_start_matches("./").to_owned()
}

/// A recall request.
#[derive(Debug, Clone)]
pub struct RecallRequest {
    pub about: About,
    /// Only activity newer than this (epoch ms).
    pub since_ms: Option<i64>,
    /// Episodes (or rows) to return.
    pub limit: usize,
}

/// Recall from the store at `db_path` (opened read-only) about the
/// repository containing `project_root`, within `deadline`.
pub fn recall_at(
    db_path: &Path,
    project_root: &Path,
    req: &RecallRequest,
    deadline: Duration,
) -> Result<RecallReport, HistoryError> {
    let mut report = RecallReport::new(&req.about);
    if !db_path.is_file() {
        report.note = Some("no agent history recorded yet (run `codegraph history ingest`)".into());
        return Ok(report);
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(db_path, flags)?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version < CURRENT_VERSION {
        report.note =
            Some("agent history is on an older schema; it is upgraded by the next ingest".into());
        return Ok(report);
    }
    let Some(repo_root) = RepoLocator::new().repo_of_dir(project_root) else {
        report.note = Some("not inside a git repository".into());
        return Ok(report);
    };
    let probe = IndexProbe::open(&repo_root);
    let interrupt = conn.get_interrupt_handle();
    let (done, wait) = mpsc::channel::<()>();
    let watchdog = std::thread::spawn(move || {
        if wait.recv_timeout(deadline) == Err(mpsc::RecvTimeoutError::Timeout) {
            interrupt.interrupt();
        }
    });
    let result = recall(&conn, &repo_root, probe.as_ref(), req, now_ms());
    let _ = done.send(());
    let _ = watchdog.join();
    match result {
        Ok(r) => Ok(r),
        Err(rusqlite::Error::SqliteFailure(e, _))
            if e.code == rusqlite::ErrorCode::OperationInterrupted =>
        {
            report.note = Some("recall timed out; narrow `about` or `since`".into());
            report.truncated = true;
            Ok(report)
        }
        Err(e) => Err(e.into()),
    }
}

/// Run a recall on an open store connection.
pub(crate) fn recall(
    conn: &Connection,
    repo_root: &Path,
    probe: Option<&IndexProbe>,
    req: &RecallRequest,
    now: i64,
) -> rusqlite::Result<RecallReport> {
    let mut report = RecallReport::new(&req.about);
    let root_text = crate::history::redact(&repo_root.to_string_lossy()).0;
    let repo: Option<i64> = conn
        .query_row(
            "SELECT id FROM repos WHERE root = ?1",
            params![root_text],
            |r| r.get(0),
        )
        .optional()?;
    let Some(repo) = repo else {
        report.note = Some("no agent sessions recorded for this repository yet".into());
        return Ok(report);
    };
    let q = Queries {
        conn,
        repo,
        probe,
        since: req.since_ms.unwrap_or(0),
        limit: req.limit.clamp(1, 20) as i64,
        now,
    };
    // An absolute path names a file of this repository by its full path.
    let relative = |p: &str| -> String {
        match Path::new(p).strip_prefix(repo_root) {
            Ok(rel) if Path::new(p).is_absolute() => rel.to_string_lossy().into_owned(),
            _ => p.to_owned(),
        }
    };
    let about = match &req.about {
        About::Path(p) => About::Path(relative(p)),
        About::Cochange(Some(p)) => About::Cochange(Some(relative(p))),
        other => other.clone(),
    };
    match &about {
        About::Last => report.episodes = q.episodes(None)?,
        About::Path(prefix) => report.episodes = q.episodes(Some(prefix))?,
        About::Symbol(name) => {
            let (symbols, episodes) = q.symbol(name)?;
            report.symbols = symbols;
            report.episodes = episodes;
        }
        About::Failures => report.failures = q.failures(false)?,
        About::Cochange(path) => report.cochange = q.cochange(path.as_deref())?,
    }
    if report.is_empty() {
        report.note = Some(match &req.about {
            About::Path(p) => format!("no recorded session touched `{p}`"),
            About::Symbol(s) => format!("no recorded session looked up `{s}`"),
            About::Failures => "no failing build or test runs recorded".into(),
            About::Cochange(_) => "no co-edited files recorded".into(),
            About::Last => "no episodes recorded".into(),
        });
    }
    report.fit(RECALL_BUDGET);
    Ok(report)
}

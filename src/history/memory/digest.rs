//! The per-repository digest the prompt hook shows on a session's first
//! prompt: the last episodes, the hot files, and failures not fixed since.
//! Built after each ingest (never in the hook) and read back with one
//! indexed lookup.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use super::index_probe::IndexProbe;
use super::queries::Queries;
use super::report::{EpisodeRow, FailureRow};
use crate::history::schema::CURRENT_VERSION;

/// Hard bound on a digest body.
pub const DIGEST_BUDGET: usize = 1024;

const DAY_MS: i64 = 86_400_000;

/// How long a hook-time read waits on an ingest's checkpoint.
const READ_BUSY: std::time::Duration = std::time::Duration::from_millis(50);

/// Build the digest of `repo` (an ingest-time rollup). `None` when the
/// repository has no episodes.
pub(crate) fn build(
    conn: &Connection,
    repo: i64,
    repo_root: &Path,
    now: i64,
) -> rusqlite::Result<Option<String>> {
    let probe = IndexProbe::open(repo_root);
    let base = Queries {
        conn,
        repo,
        probe: probe.as_ref(),
        since: 0,
        limit: 3,
        now,
    };
    let episodes = base.episodes(None)?;
    if episodes.is_empty() {
        return Ok(None);
    }
    let hot = Queries {
        since: now - 30 * DAY_MS,
        ..base
    }
    .hot_files("", 6)?;
    let failures = Queries {
        since: now - 14 * DAY_MS,
        limit: 2,
        ..base
    }
    .failures(true)?;
    Ok(Some(render(&episodes, &hot, &failures)))
}

/// Render within [`DIGEST_BUDGET`], dropping detail until it fits.
pub(crate) fn render(
    episodes: &[EpisodeRow],
    hot: &[(String, i64)],
    failures: &[FailureRow],
) -> String {
    const LEVELS: &[(usize, usize, usize)] = &[
        (4, 6, 2),
        (3, 4, 2),
        (2, 3, 1),
        (1, 2, 1),
        (0, 0, 1),
        (0, 0, 0),
    ];
    let mut body = String::new();
    for &(files, hot_n, fail_n) in LEVELS {
        body = render_at(episodes, hot, failures, files, hot_n, fail_n);
        if body.len() <= DIGEST_BUDGET {
            return body;
        }
    }
    let mut end = DIGEST_BUDGET;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    body.truncate(end);
    body
}

fn render_at(
    episodes: &[EpisodeRow],
    hot: &[(String, i64)],
    failures: &[FailureRow],
    files: usize,
    hot_n: usize,
    fail_n: usize,
) -> String {
    let mut out = String::from(
        "Earlier agent sessions in this repository (more: the codegraph_recall tool or \
         `codegraph history recall <path|symbol:NAME|failures|cochange>`):\n",
    );
    for e in episodes {
        out.push_str(&format!(
            "- {} ago, {}, {} calls, {}",
            e.ago, e.source, e.calls, e.outcome
        ));
        let shown: Vec<String> = e
            .files
            .iter()
            .take(files)
            .map(|f| {
                let mark = if f.unchanged == Some(true) {
                    " (unchanged)"
                } else {
                    ""
                };
                format!("{} {}{mark}", f.op, f.path)
            })
            .collect();
        if !shown.is_empty() {
            out.push_str(": ");
            out.push_str(&shown.join(", "));
        }
        let more = e.files.len().saturating_sub(files) + e.more_files;
        if more > 0 && files > 0 {
            out.push_str(&format!(" (+{more})"));
        }
        out.push('\n');
    }
    if hot_n > 0 && !hot.is_empty() {
        let list: Vec<String> = hot
            .iter()
            .take(hot_n)
            .map(|(p, n)| format!("{p} ({n})"))
            .collect();
        out.push_str(&format!("Hot files, 30d (episodes): {}\n", list.join(", ")));
    }
    let open: Vec<String> = failures
        .iter()
        .take(fail_n)
        .map(|f| {
            let codes = if f.codes.is_empty() {
                String::new()
            } else {
                format!(" {}", f.codes.join(","))
            };
            format!("`{}`{codes} {} ago ({}×)", f.command, f.ago, f.repeats)
        })
        .collect();
    if !open.is_empty() {
        out.push_str(&format!("Failing, not fixed since: {}\n", open.join("; ")));
    }
    out
}

/// What the prompt hook found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DigestStatus {
    /// No history store yet.
    NoStore,
    /// The store predates this build's schema (the next ingest upgrades it).
    Outdated,
    /// No digest for this repository (yet).
    Absent,
    Ready {
        body: String,
        built_at: i64,
    },
}

/// A digest read, plus when the last ingest finished (for the hook's
/// "refresh in the background" decision).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestRead {
    pub status: DigestStatus,
    pub last_run_ms: Option<i64>,
}

/// When the last ingest into the store at `db_path` finished (read-only;
/// creates nothing).
pub fn last_ingest_ms(db_path: &Path) -> Option<i64> {
    if !db_path.is_file() {
        return None;
    }
    let conn = crate::atlas::ro::open_read_only(db_path, READ_BUSY).ok()?;
    conn.query_row(
        "SELECT value FROM ingest_state WHERE source = '_meta' AND key = 'last_run'",
        [],
        |r| r.get::<_, String>(0),
    )
    .ok()?
    .parse()
    .ok()
}

/// Read the digest of the repository at `repo_root` — read-only, one
/// indexed lookup. Never creates (not even SQLite's side files), migrates
/// or writes anything.
pub fn read_digest(db_path: &Path, repo_root: &Path) -> DigestRead {
    let absent = |status| DigestRead {
        status,
        last_run_ms: None,
    };
    if !db_path.is_file() {
        return absent(DigestStatus::NoStore);
    }
    let Ok(conn) = crate::atlas::ro::open_read_only(db_path, READ_BUSY) else {
        return absent(DigestStatus::NoStore);
    };
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap_or(0);
    if version < CURRENT_VERSION {
        return absent(DigestStatus::Outdated);
    }
    let last_run_ms = conn
        .query_row(
            "SELECT value FROM ingest_state WHERE source = '_meta' AND key = 'last_run'",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok());
    let root = crate::history::redact(&repo_root.to_string_lossy()).0;
    let found: Option<(String, i64)> = conn
        .query_row(
            "SELECT d.body, d.built_at FROM repo_digests d
             WHERE d.repo_id = (SELECT id FROM repos WHERE root = ?1)",
            params![root],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .ok()
        .flatten();
    DigestRead {
        status: match found {
            Some((body, built_at)) => DigestStatus::Ready { body, built_at },
            None => DigestStatus::Absent,
        },
        last_run_ms,
    }
}

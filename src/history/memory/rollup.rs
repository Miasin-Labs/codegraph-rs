//! Per-repository rollups run at the end of an ingest (never in a request
//! path): git co-change pairs merged into `coedits`, and the digest the
//! prompt hook reads.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

use super::digest;
use super::writer::upsert_file;
use crate::history::redact::redact;
use crate::history::store::HistoryError;

/// Commits mined for co-change.
const GIT_COMMITS: usize = 300;
/// Commits touching more files than this are bulk changes, not coupling.
const MAX_COMMIT_FILES: usize = 30;
/// Pairs kept per repository.
const MAX_PAIRS: usize = 1_000;
/// Minimum time between two git minings of one repository.
const GIT_EVERY_MS: i64 = 6 * 3_600_000;
/// `git log` gets this long.
const GIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Rebuild the rollups of `repo`: git co-change (when `git` and due) and
/// the digest. Each in its own transaction.
pub(crate) fn roll_up(
    conn: &Connection,
    repo: i64,
    git: bool,
    now: i64,
) -> Result<(), HistoryError> {
    let root: Option<String> = conn
        .query_row("SELECT root FROM repos WHERE id = ?1", params![repo], |r| {
            r.get(0)
        })
        .optional()?;
    let Some(root) = root.map(PathBuf::from) else {
        return Ok(());
    };
    if git && git_due(conn, repo, now)? {
        if let Some(pairs) = git_pairs(&root) {
            let tx = conn.unchecked_transaction()?;
            store_git_pairs(&tx, repo, &pairs)?;
            tx.execute(
                "INSERT OR REPLACE INTO ingest_state (source, key, value) VALUES ('_repo', ?1, ?2)",
                params![format!("git:{repo}"), now.to_string()],
            )?;
            tx.commit()?;
        }
    }
    let body = digest::build(conn, repo, &root, now)?;
    let tx = conn.unchecked_transaction()?;
    match body {
        Some(body) => {
            tx.execute(
                "INSERT OR REPLACE INTO repo_digests (repo_id, built_at, body) VALUES (?1, ?2, ?3)",
                params![repo, now, body],
            )?;
        }
        None => {
            tx.execute("DELETE FROM repo_digests WHERE repo_id = ?1", params![repo])?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn git_due(conn: &Connection, repo: i64, now: i64) -> rusqlite::Result<bool> {
    let last: Option<String> = conn
        .query_row(
            "SELECT value FROM ingest_state WHERE source = '_repo' AND key = ?1",
            params![format!("git:{repo}")],
            |r| r.get(0),
        )
        .optional()?;
    Ok(last
        .and_then(|v| v.parse::<i64>().ok())
        .is_none_or(|t| now - t >= GIT_EVERY_MS))
}

/// File pairs changed together in at least two of the last
/// [`GIT_COMMITS`] non-merge commits. `None` when git is unavailable or
/// too slow.
pub(crate) fn git_pairs(root: &Path) -> Option<Vec<(String, String, i64)>> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["log", "--no-merges", "--name-only", "--format=%x1e"])
        .arg(format!("-n{GIT_COMMITS}"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let ok = stdout.read_to_string(&mut text).is_ok();
        let _ = tx.send(ok.then_some(text));
    });
    let Ok(Some(text)) = rx.recv_timeout(GIT_TIMEOUT) else {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    if !child.wait().ok()?.success() {
        return None;
    }
    let mut counts: HashMap<(String, String), i64> = HashMap::new();
    for commit in text.split('\u{1e}') {
        let mut files: Vec<&str> = commit
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        if files.len() < 2 || files.len() > MAX_COMMIT_FILES {
            continue;
        }
        files.sort_unstable();
        files.dedup();
        for (i, a) in files.iter().enumerate() {
            for b in &files[i + 1..] {
                *counts
                    .entry(((*a).to_owned(), (*b).to_owned()))
                    .or_default() += 1;
            }
        }
    }
    let mut pairs: Vec<(String, String, i64)> = counts
        .into_iter()
        .filter(|(_, n)| *n >= 2)
        .map(|((a, b), n)| (a, b, n))
        .collect();
    pairs.sort_by(|x, y| {
        y.2.cmp(&x.2)
            .then_with(|| x.0.cmp(&y.0))
            .then_with(|| x.1.cmp(&y.1))
    });
    pairs.truncate(MAX_PAIRS);
    Some(pairs)
}

fn store_git_pairs(
    conn: &Connection,
    repo: i64,
    pairs: &[(String, String, i64)],
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE coedits SET commits = 0 WHERE repo_id = ?1",
        params![repo],
    )?;
    let mut upsert = conn.prepare_cached(
        "INSERT INTO coedits (repo_id, file_a, file_b, commits) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(repo_id, file_a, file_b) DO UPDATE SET commits = excluded.commits",
    )?;
    for (a, b, n) in pairs {
        let fa = upsert_file(conn, repo, &redact(a).0)?;
        let fb = upsert_file(conn, repo, &redact(b).0)?;
        let (x, y) = if fa < fb { (fa, fb) } else { (fb, fa) };
        if x != y {
            upsert.execute(params![repo, x, y, n])?;
        }
    }
    conn.execute(
        "DELETE FROM coedits WHERE repo_id = ?1 AND episodes = 0 AND commits = 0",
        params![repo],
    )?;
    Ok(())
}

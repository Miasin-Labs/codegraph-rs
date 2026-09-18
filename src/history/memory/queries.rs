//! The indexed, `LIMIT`ed queries behind a recall and the digest.

use rusqlite::{Connection, OptionalExtension, params};

use super::index_probe::IndexProbe;
use super::report::{EpisodeRow, FailureRow, FileTouch, PairRow, SymbolRow};
use super::writer::hashed_ident;
use crate::history::activity::Op;
use crate::history::time::ago;

/// Episodes scanned (newest first) when filtering by path.
const SCAN_EPISODES: i64 = 2_000;
/// Files listed per episode.
const FILES_PER_EPISODE: usize = 6;
/// A file modified this long after a touch still counts as what it saw.
const SETTLE_MS: i64 = 5_000;

/// The indexed queries behind a recall (and the digest).
#[derive(Clone, Copy)]
pub(crate) struct Queries<'a> {
    pub conn: &'a Connection,
    pub repo: i64,
    pub probe: Option<&'a IndexProbe>,
    pub since: i64,
    pub limit: i64,
    pub now: i64,
}

impl Queries<'_> {
    /// Recent episodes (touching `prefix`, when given), newest first.
    pub(crate) fn episodes(&self, prefix: Option<&str>) -> rusqlite::Result<Vec<EpisodeRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT e.id, e.started_at, e.calls, e.outcome, s.source
             FROM episodes e JOIN sessions s ON s.id = e.session_id
             WHERE e.repo_id = ?1 AND e.started_at >= ?2 AND e.calls > 0
             ORDER BY e.started_at DESC LIMIT ?3",
        )?;
        let scan = if prefix.is_some() {
            SCAN_EPISODES
        } else {
            self.limit
        };
        let candidates: Vec<(i64, i64, i64, Option<String>, String)> = stmt
            .query_map(params![self.repo, self.since, scan], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut out = Vec::new();
        for (id, started, calls, outcome, source) in candidates {
            let (files, more, edited) = self.episode_files(id, self.repo, prefix)?;
            if prefix.is_some() && files.is_empty() {
                continue;
            }
            out.push(EpisodeRow {
                started_ms: started,
                ago: ago(started, self.now),
                source,
                calls,
                outcome: outcome
                    .unwrap_or_else(|| if edited { "edited" } else { "explored" }.into()),
                files,
                more_files: more,
            });
            if out.len() as i64 >= self.limit {
                break;
            }
        }
        Ok(out)
    }

    /// Files of `file_repo` an episode touched (under `prefix`): strongest
    /// op first, then most touched; whether it edited anything at all.
    fn episode_files(
        &self,
        episode: i64,
        file_repo: i64,
        prefix: Option<&str>,
    ) -> rusqlite::Result<(Vec<FileTouch>, usize, bool)> {
        let (lo, hi) = prefix_range(prefix.unwrap_or(""));
        let mut stmt = self.conn.prepare_cached(
            "SELECT f.path, t.op, t.n, t.fingerprint, t.last_ts, t.op = 'e'
             FROM touches t JOIN files f ON f.id = t.file_id
             WHERE t.episode_id = ?1 AND f.repo_id = ?2
             ORDER BY t.op = 'e' DESC, t.op = 'r' DESC, t.n DESC, f.path",
        )?;
        let rows: Vec<(String, String, i64, Option<String>, Option<i64>, bool)> = stmt
            .query_map(params![episode, file_repo], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let edited = rows.iter().any(|r| r.5);
        let mut files: Vec<FileTouch> = Vec::new();
        let mut more = 0;
        for (path, op, n, fingerprint, last_ts, _) in rows {
            if path.as_str() < lo.as_str() || (!hi.is_empty() && path.as_str() >= hi.as_str()) {
                continue;
            }
            if files.iter().any(|f| f.path == path) {
                continue; // weaker op of a file already listed
            }
            if files.len() >= FILES_PER_EPISODE {
                more += 1;
                continue;
            }
            let unchanged = if file_repo == self.repo {
                self.unchanged(&path, fingerprint.as_deref(), last_ts)
            } else {
                None
            };
            files.push(FileTouch {
                path,
                op: op_name(&op),
                n,
                unchanged,
            });
        }
        Ok((files, more, edited))
    }

    fn edited(&self, episode: i64) -> rusqlite::Result<bool> {
        self.conn
            .prepare_cached(
                "SELECT EXISTS(SELECT 1 FROM touches WHERE episode_id = ?1 AND op = 'e')",
            )?
            .query_row(params![episode], |r| r.get(0))
    }

    /// Whether `path` is still what a touch at `last_ts` saw.
    fn unchanged(
        &self,
        path: &str,
        fingerprint: Option<&str>,
        last_ts: Option<i64>,
    ) -> Option<bool> {
        let probe = self.probe?;
        let (hash, modified) = probe.file_state(path)?;
        match fingerprint {
            Some(fp) => Some(fp == hash),
            None => last_ts.map(|t| modified <= t + SETTLE_MS),
        }
    }

    /// Lookups of `name` (in the clear, or by its hash).
    pub(crate) fn symbol(&self, name: &str) -> rusqlite::Result<(Vec<SymbolRow>, Vec<EpisodeRow>)> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT l.episode_id, l.n, f.path, l.found_line, e.started_at, e.calls, e.outcome, s.source
             FROM idents i
             JOIN lookups l ON l.ident_id = i.id
             JOIN episodes e ON e.id = l.episode_id
             JOIN sessions s ON s.id = e.session_id
             LEFT JOIN files f ON f.id = l.found_file_id
             WHERE i.repo_id = ?1 AND i.name IN (?2, ?3) AND e.started_at >= ?4
             ORDER BY e.started_at DESC LIMIT 200",
        )?;
        type Row = (
            i64,
            i64,
            Option<String>,
            Option<i64>,
            i64,
            i64,
            Option<String>,
            String,
        );
        let rows: Vec<Row> = stmt
            .query_map(
                params![self.repo, name, hashed_ident(name), self.since],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                    ))
                },
            )?
            .collect::<rusqlite::Result<_>>()?;
        if rows.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let mut found: Vec<(String, usize)> = Vec::new();
        for (_, _, path, line, ..) in &rows {
            if let Some(path) = path {
                let loc = match line {
                    Some(l) => format!("{path}:{l}"),
                    None => path.clone(),
                };
                match found.iter_mut().find(|(l, _)| *l == loc) {
                    Some((_, n)) => *n += 1,
                    None => found.push((loc, 1)),
                }
            }
        }
        found.sort_by_key(|a| std::cmp::Reverse(a.1));
        let symbol = SymbolRow {
            name: name.to_owned(),
            lookups: rows.iter().map(|r| r.1).sum(),
            episodes: rows.len() as i64,
            found: found.into_iter().take(3).map(|(l, _)| l).collect(),
            ago: ago(rows[0].4, self.now),
        };
        let mut episodes = Vec::new();
        for (id, _, path, _, started, calls, outcome, source) in
            rows.into_iter().take(self.limit as usize)
        {
            let edited = self.edited(id)?;
            let files = path
                .map(|p| {
                    let unchanged = self.unchanged(&p, None, Some(started));
                    vec![FileTouch {
                        path: p,
                        op: "read",
                        n: 1,
                        unchanged,
                    }]
                })
                .unwrap_or_default();
            episodes.push(EpisodeRow {
                started_ms: started,
                ago: ago(started, self.now),
                source,
                calls,
                outcome: outcome
                    .unwrap_or_else(|| if edited { "edited" } else { "explored" }.into()),
                files,
                more_files: 0,
            });
        }
        Ok((vec![symbol], episodes))
    }

    /// Recent failing runs, newest first, one row per (command, failure);
    /// `open_only` keeps those no later run of the command fixed.
    pub(crate) fn failures(&self, open_only: bool) -> rusqlite::Result<Vec<FailureRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT o.kind, o.template, o.err_codes, o.err_sig, o.ts
             FROM outcomes o
             WHERE o.repo_id = ?1 AND o.ok = 0 AND o.ts >= ?2 AND o.kind <> 'commit'
             ORDER BY o.ts DESC LIMIT 200",
        )?;
        let rows: Vec<(String, String, Option<String>, Option<String>, i64)> = stmt
            .query_map(params![self.repo, self.since], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut fixed = self.conn.prepare_cached(
            "SELECT 1 FROM outcomes
             WHERE repo_id = ?1 AND kind = ?2 AND ts > ?3 AND ok = 1
               AND (template = ?4 OR template LIKE ?5)
             LIMIT 1",
        )?;
        let mut out: Vec<(FailureRow, String)> = Vec::new();
        for (kind, template, codes, sig, ts) in rows {
            let key = format!("{template}\0{}", sig.as_deref().unwrap_or(""));
            if let Some((row, _)) = out.iter_mut().find(|(_, k)| *k == key) {
                row.repeats += 1;
                continue;
            }
            let family = template.split(' ').take(2).collect::<Vec<_>>().join(" ");
            let fixed_later = fixed
                .query_row(
                    params![self.repo, kind, ts, template, format!("{family}%")],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if open_only && fixed_later {
                continue;
            }
            out.push((
                FailureRow {
                    ts_ms: ts,
                    ago: ago(ts, self.now),
                    kind,
                    command: template,
                    codes: codes
                        .map(|c| c.split(',').map(str::to_owned).collect())
                        .unwrap_or_default(),
                    repeats: 1,
                    fixed_later,
                },
                key,
            ));
            if out.len() as i64 >= self.limit.max(5) {
                break;
            }
        }
        Ok(out.into_iter().map(|(row, _)| row).collect())
    }

    /// Co-edited pairs (sessions + git), strongest first; for one file or a
    /// path prefix when given.
    pub(crate) fn cochange(&self, path: Option<&str>) -> rusqlite::Result<Vec<PairRow>> {
        let limit = self.limit.max(10);
        let map = |r: &rusqlite::Row<'_>| {
            Ok(PairRow {
                a: r.get(0)?,
                b: r.get(1)?,
                episodes: r.get(2)?,
                commits: r.get(3)?,
            })
        };
        let Some(path) = path else {
            let mut stmt = self.conn.prepare_cached(
                "SELECT fa.path, fb.path, c.episodes, c.commits FROM coedits c
                 JOIN files fa ON fa.id = c.file_a JOIN files fb ON fb.id = c.file_b
                 WHERE c.repo_id = ?1 AND (c.episodes + c.commits) >= 2
                 ORDER BY c.episodes * 2 + c.commits DESC, fa.path LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![self.repo, limit], map)?.collect();
            return rows;
        };
        let (lo, hi) = prefix_range(path);
        let mut stmt = self.conn.prepare_cached(
            "SELECT fa.path, fb.path, c.episodes, c.commits FROM coedits c
             JOIN files fa ON fa.id = c.file_a JOIN files fb ON fb.id = c.file_b
             WHERE c.repo_id = ?1 AND (c.file_a IN (SELECT id FROM files WHERE repo_id = ?1 AND path >= ?2 AND path < ?3 LIMIT 50)
                                   OR c.file_b IN (SELECT id FROM files WHERE repo_id = ?1 AND path >= ?2 AND path < ?3 LIMIT 50))
             ORDER BY c.episodes * 2 + c.commits DESC, fa.path LIMIT ?4",
        )?;
        let hi = if hi.is_empty() {
            "\u{10ffff}".to_owned()
        } else {
            hi
        };
        let rows = stmt
            .query_map(params![self.repo, lo, hi, limit], map)?
            .collect();
        rows
    }

    /// Files (under `prefix`) touched in the most episodes since `since`.
    pub(crate) fn hot_files(
        &self,
        prefix: &str,
        limit: i64,
    ) -> rusqlite::Result<Vec<(String, i64)>> {
        let (lo, hi) = bounded_range(prefix);
        let mut stmt = self.conn.prepare_cached(
            "SELECT f.path, COUNT(DISTINCT t.episode_id) AS eps
             FROM episodes e JOIN touches t ON t.episode_id = e.id JOIN files f ON f.id = t.file_id
             WHERE e.repo_id = ?1 AND e.started_at >= ?2 AND f.repo_id = ?1 AND t.op <> 's'
               AND f.path >= ?4 AND f.path < ?5
             GROUP BY f.id ORDER BY eps DESC, f.path LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![self.repo, self.since, limit, lo, hi], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect();
        rows
    }

    /// When anything last happened in this repository (under `prefix`):
    /// its own episodes' last calls, and any session's touches of its
    /// files (an agent in another checkout editing it counts).
    pub(crate) fn last_activity(&self, prefix: &str) -> rusqlite::Result<Option<i64>> {
        let (lo, hi) = bounded_range(prefix);
        let touched: Option<i64> = self
            .conn
            .prepare_cached(
                "SELECT MAX(t.last_ts) FROM files f JOIN touches t ON t.file_id = f.id
                 WHERE f.repo_id = ?1 AND f.path >= ?2 AND f.path < ?3",
            )?
            .query_row(params![self.repo, lo, hi], |r| r.get(0))?;
        if !prefix.is_empty() {
            return Ok(touched);
        }
        let own: Option<i64> = self
            .conn
            .prepare_cached(
                "SELECT MAX(COALESCE(ended_at, started_at)) FROM episodes
                 WHERE repo_id = ?1 AND calls > 0",
            )?
            .query_row(params![self.repo], |r| r.get(0))?;
        Ok(own.max(touched))
    }

    /// Root sessions since `since` with an episode in this repository or a
    /// touch of its files (only the touches under a non-empty `prefix`).
    pub(crate) fn sessions(&self, prefix: &str) -> rusqlite::Result<i64> {
        let (lo, hi) = bounded_range(prefix);
        let sql = if prefix.is_empty() {
            "SELECT COUNT(*) FROM (
                 SELECT session_id FROM episodes WHERE repo_id = ?1 AND started_at >= ?2 AND calls > 0
                 UNION
                 SELECT e.session_id FROM files f JOIN touches t ON t.file_id = f.id
                 JOIN episodes e ON e.id = t.episode_id
                 WHERE f.repo_id = ?1 AND f.path >= ?3 AND f.path < ?4 AND t.last_ts >= ?2)"
        } else {
            "SELECT COUNT(DISTINCT e.session_id) FROM files f JOIN touches t ON t.file_id = f.id
             JOIN episodes e ON e.id = t.episode_id
             WHERE f.repo_id = ?1 AND f.path >= ?3 AND f.path < ?4 AND t.last_ts >= ?2"
        };
        self.conn
            .prepare_cached(sql)?
            .query_row(params![self.repo, self.since, lo, hi], |r| r.get(0))
    }

    /// Latest edit since `since` of a file of this repository (under
    /// `prefix`) — by any session, or only by sessions of `by_repo`.
    pub(crate) fn last_edit(
        &self,
        prefix: &str,
        by_repo: Option<i64>,
    ) -> rusqlite::Result<Option<i64>> {
        let (lo, hi) = bounded_range(prefix);
        self.conn
            .prepare_cached(
                "SELECT MAX(t.last_ts) FROM files f JOIN touches t ON t.file_id = f.id
                 JOIN episodes e ON e.id = t.episode_id
                 WHERE f.repo_id = ?1 AND f.path >= ?2 AND f.path < ?3 AND t.op = 'e'
                   AND t.last_ts >= ?4 AND (?5 IS NULL OR e.repo_id = ?5)",
            )?
            .query_row(params![self.repo, lo, hi, self.since, by_repo], |r| {
                r.get(0)
            })
    }

    /// Episodes of this repository that touched files of `file_repo` under
    /// `prefix` (another checkout's sessions working on a shared crate),
    /// newest first.
    pub(crate) fn episodes_touching(
        &self,
        file_repo: i64,
        prefix: &str,
    ) -> rusqlite::Result<Vec<EpisodeRow>> {
        let (lo, hi) = bounded_range(prefix);
        let mut stmt = self.conn.prepare_cached(
            "SELECT e.id, MAX(e.started_at), MAX(e.calls), MAX(e.outcome), MAX(s.source)
             FROM files f JOIN touches t ON t.file_id = f.id
             JOIN episodes e ON e.id = t.episode_id JOIN sessions s ON s.id = e.session_id
             WHERE f.repo_id = ?1 AND f.path >= ?2 AND f.path < ?3
               AND e.repo_id = ?4 AND e.started_at >= ?5
             GROUP BY e.id ORDER BY MAX(e.started_at) DESC LIMIT ?6",
        )?;
        let rows: Vec<(i64, i64, i64, Option<String>, String)> = stmt
            .query_map(
                params![file_repo, lo, hi, self.repo, self.since, self.limit],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?
            .collect::<rusqlite::Result<_>>()?;
        let scope = (!prefix.is_empty()).then_some(prefix);
        let mut out = Vec::with_capacity(rows.len());
        for (id, started, calls, outcome, source) in rows {
            let (files, more, _) = self.episode_files(id, file_repo, scope)?;
            let edited = self.edited(id)?;
            out.push(EpisodeRow {
                started_ms: started,
                ago: ago(started, self.now),
                source,
                calls,
                outcome: outcome
                    .unwrap_or_else(|| if edited { "edited" } else { "explored" }.into()),
                files,
                more_files: more,
            });
        }
        Ok(out)
    }
}

/// `[lo, hi)` bounds of paths starting with `prefix`, with a finite upper
/// bound for the empty prefix (every path).
fn bounded_range(prefix: &str) -> (String, String) {
    let (lo, hi) = prefix_range(prefix);
    let hi = if hi.is_empty() {
        "\u{10ffff}".to_owned()
    } else {
        hi
    };
    (lo, hi)
}

/// `[lo, hi)` bounds of paths starting with `prefix` (`hi` empty = no bound).
fn prefix_range(prefix: &str) -> (String, String) {
    if prefix.is_empty() {
        return (String::new(), String::new());
    }
    let mut hi = prefix.to_owned();
    hi.push('\u{10ffff}');
    (prefix.to_owned(), hi)
}

fn op_name(op: &str) -> &'static str {
    match Op::from_str(op) {
        Some(Op::Edit) => "edit",
        Some(Op::Read) => "read",
        _ => "search",
    }
}

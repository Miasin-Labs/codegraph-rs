//! Where an identifier was found: a lookup (grep pattern, symbol argument)
//! waits a few calls for the file read or edited next — for an indexed
//! symbol, the file that defines it.

use rusqlite::params;

use super::{MemoryWriter, hashed_ident};
use crate::history::activity::Op;
use crate::history::redact::redact;
use crate::history::store::HistoryError;

/// Calls after a search within which a read counts as "where it was found".
const LOOKUP_WINDOW: u8 = 5;

pub(super) struct PendingLookup {
    pub(super) ident: i64,
    /// The identifier when the index resolved it (its definition can be
    /// located in the file read next).
    pub(super) name: Option<String>,
    /// First file read since, for a resolved name no read file defines.
    pub(super) fallback: Option<(i64, Option<u32>)>,
    pub(super) ttl: u8,
}

/// A file a call read, edited or searched: where a pending lookup may
/// have been found.
pub(super) struct Candidate {
    pub(super) repo: i64,
    pub(super) file: i64,
    pub(super) rel: String,
    pub(super) line: Option<u32>,
    pub(super) op: Op,
}

impl MemoryWriter<'_> {
    /// Record the identifiers a call looked up: in the clear when the
    /// repo's index knows the name, else as a hash.
    pub(super) fn lookups(
        &mut self,
        episode: i64,
        repo: i64,
        idents: &[String],
        ts: Option<i64>,
    ) -> Result<(), HistoryError> {
        for ident in idents {
            let resolved = self.probe(repo).is_some_and(|p| p.has_symbol(ident));
            let stored = if resolved {
                redact(ident).0
            } else {
                hashed_ident(ident)
            };
            let id = self.ident_id(repo, &stored)?;
            self.conn
                .prepare_cached(
                    "INSERT INTO lookups (episode_id, ident_id, n, last_ts) VALUES (?1, ?2, 1, ?3)
                     ON CONFLICT(episode_id, ident_id) DO UPDATE SET
                         n = n + 1,
                         last_ts = COALESCE(MAX(last_ts, excluded.last_ts), last_ts, excluded.last_ts)",
                )?
                .execute(params![episode, id, ts])?;
            let pending = self.pending.entry(episode).or_default();
            pending.retain(|p| p.ident != id);
            pending.push(PendingLookup {
                ident: id,
                name: resolved.then(|| stored.clone()),
                fallback: None,
                ttl: LOOKUP_WINDOW + 1,
            });
        }
        Ok(())
    }

    /// A read or edit shortly after a lookup is where the identifier was
    /// found: for an indexed symbol, the file defining it (and the line);
    /// else — or when no file read in the window defines it — the first
    /// file read.
    pub(super) fn resolve_lookups(
        &mut self,
        episode: i64,
        candidates: &[Candidate],
    ) -> Result<(), HistoryError> {
        let Some(mut pending) = self.pending.remove(&episode) else {
            return Ok(());
        };
        let first = candidates.first().map(|c| (c.file, c.line));
        let mut still = Vec::with_capacity(pending.len());
        for mut p in pending.drain(..) {
            let hit = match &p.name {
                Some(name) => candidates.iter().find_map(|c| {
                    self.probe(c.repo)
                        .and_then(|probe| probe.symbol_line(name, &c.rel))
                        .map(|l| (c.file, Some(l)))
                }),
                None => first,
            };
            if let Some((file, line)) = hit {
                self.found(episode, p.ident, file, line)?;
                continue;
            }
            if p.fallback.is_none() {
                // Not where it's defined: the file, not a line.
                p.fallback = first.map(|(file, _)| (file, None));
            }
            p.ttl = p.ttl.saturating_sub(1);
            if p.ttl > 0 {
                still.push(p);
            } else if let Some((file, line)) = p.fallback {
                self.found(episode, p.ident, file, line)?;
            }
        }
        if !still.is_empty() {
            self.pending.insert(episode, still);
        }
        Ok(())
    }

    fn found(
        &self,
        episode: i64,
        ident: i64,
        file: i64,
        line: Option<u32>,
    ) -> Result<(), HistoryError> {
        self.conn
            .prepare_cached(
                "UPDATE lookups SET found_file_id = ?3, found_line = ?4
                 WHERE episode_id = ?1 AND ident_id = ?2 AND found_file_id IS NULL",
            )?
            .execute(params![episode, ident, file, line])?;
        Ok(())
    }

    /// Settle what is still pending at the end of a pass: lookups whose
    /// window hasn't closed keep the first file read after them.
    pub(crate) fn finish(&mut self) -> Result<(), HistoryError> {
        let pending: Vec<(i64, Vec<PendingLookup>)> = self.pending.drain().collect();
        for (episode, lookups) in pending {
            for p in lookups {
                if let Some((file, line)) = p.fallback {
                    self.found(episode, p.ident, file, line)?;
                }
            }
        }
        Ok(())
    }
}

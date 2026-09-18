//! The history × atlas join (federation phase 4): the cross-session memory
//! (`history.db`) read per atlas project (`atlas.db`), and across the
//! projects the atlas links together.
//!
//! Both stores key a project by a canonical path, but not the same one:
//! history folds a linked worktree into its repository's main checkout
//! ([`crate::history::repo`]), the atlas keeps one row per checkout and
//! groups worktrees by `repo_root`. [`HistoryScope`] reconciles them — one
//! history repository maps to every atlas checkout of that repository,
//! and a project nested inside a checkout narrows to its path prefix.
//!
//! Everything here reads: both stores (and any project index an episode's
//! "unchanged" mark needs) are opened read-only without creating a file,
//! every query is indexed and `LIMIT`ed, and a [`crate::history::deadline`]
//! interrupts what outlasts its caller's budget. Consumers: `codegraph
//! projects list|show`, `codegraph history recall --project/--related`,
//! MCP `codegraph_recall` (`related`), and one line of the prompt hook's
//! session digest ([`linked_line`]).

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

use crate::history::schema::CURRENT_VERSION;
use crate::history::store::HistoryError;

mod hook_line;
mod linked;
mod reader;
mod recall;
mod scope;

pub(crate) use hook_line::{LINKED_LINE_MAX, linked_line};
pub use linked::{LinkedActivity, LinkedProject, LinkedQuery, Relation, linked_projects};
pub use reader::{ActivityReader, ActivitySummary, HotFile, ProjectActivity};
pub(crate) use recall::{RecallContext, add_related};
pub use scope::HistoryScope;

const DAY_MS: i64 = 86_400_000;

/// How long a reader waits on an ingest's checkpoint.
const BUSY: Duration = Duration::from_millis(200);

/// Open the history store at `db` read-only, creating nothing (not even
/// SQLite's side files). `None` when there is no store, or one older than
/// this build reads (the next ingest upgrades it).
pub(crate) fn open_history(db: &Path) -> Result<Option<Connection>, HistoryError> {
    if !db.is_file() {
        return Ok(None);
    }
    let conn = crate::atlas::ro::open_read_only(db, BUSY)?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    Ok((version >= CURRENT_VERSION).then_some(conn))
}

/// A history repository's root as the store keys it (redacted like every
/// string it keeps).
fn repo_key(root: &Path) -> String {
    crate::history::redact(&root.to_string_lossy()).0
}

/// The history repository recorded at `root`, if any.
fn repo_id(conn: &Connection, root: &Path) -> rusqlite::Result<Option<i64>> {
    use rusqlite::OptionalExtension;
    conn.prepare_cached("SELECT id FROM repos WHERE root = ?1")?
        .query_row([repo_key(root)], |r| r.get(0))
        .optional()
}

/// Keep the `limit` newest of `rows` (merged from several repositories).
fn newest<T>(mut rows: Vec<T>, at: impl Fn(&T) -> i64, limit: usize) -> Vec<T> {
    rows.sort_by_key(|row| std::cmp::Reverse(at(row)));
    rows.truncate(limit);
    rows
}

#[cfg(test)]
mod tests;

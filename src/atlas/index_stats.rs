//! Stats read from a project's own index — read-only, creating nothing in
//! its `.codegraph/` (see [`super::ro`]), and bounded in time.
//!
//! Everything but the node/edge totals comes from small tables (`files`,
//! `project_metadata`, `schema_versions`). An exact `COUNT(*)` over a
//! multi-GB `edges` table takes seconds cold, so the totals run under a
//! deadline that interrupts the query; past it they fall back to estimates
//! (`SUM(files.node_count)`, the `edges.id` span, `sqlite_stat1`) and are
//! marked inexact.

use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use super::model::LanguageCount;
use crate::directory::{get_codegraph_dir, get_directory_size};

/// Manifests the index lists (repo-relative), at most this many.
const MAX_INDEXED_MANIFESTS: usize = 2_000;

/// What a project's index says about it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexFacts {
    /// `MAX(schema_versions.version)`.
    pub schema: Option<u32>,
    pub extraction_version: Option<u32>,
    pub engine_version: Option<String>,
    pub languages: Vec<LanguageCount>,
    pub file_count: Option<u64>,
    pub node_count: Option<u64>,
    pub edge_count: Option<u64>,
    pub counts_exact: bool,
    pub index_bytes: Option<u64>,
    pub last_indexed_ms: Option<i64>,
    /// `Cargo.toml` / `package.json` / `go.mod` files the index tracks
    /// (repo-relative, `/`-separated).
    #[serde(skip)]
    pub manifests: Vec<String>,
}

/// Read `root`'s index. `count_budget` bounds the exact node/edge counts.
pub(crate) fn read_index_facts(
    root: &Path,
    count_budget: Duration,
) -> rusqlite::Result<IndexFacts> {
    let dir = get_codegraph_dir(root);
    let conn = super::ro::open_read_only(&dir.join("codegraph.db"), Duration::from_millis(250))?;
    // Proves this is an index at all; everything after is best effort.
    let schema: Option<u32> =
        conn.query_row("SELECT MAX(version) FROM schema_versions", [], |r| r.get(0))?;
    let metadata = |key: &str| -> Option<String> {
        conn.query_row(
            "SELECT value FROM project_metadata WHERE key = ?1",
            [key],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()
    };
    let mut facts = IndexFacts {
        schema,
        extraction_version: metadata("indexed_with_extraction_version")
            .and_then(|v| v.parse().ok()),
        engine_version: metadata("indexed_with_version"),
        languages: languages(&conn).unwrap_or_default(),
        last_indexed_ms: last_indexed(&conn),
        manifests: indexed_manifests(&conn).unwrap_or_default(),
        index_bytes: Some(get_directory_size(root)),
        ..IndexFacts::default()
    };
    facts.file_count = Some(facts.languages.iter().map(|l| l.files).sum());
    match with_deadline(&conn, count_budget, exact_counts) {
        Ok((nodes, edges)) => {
            facts.node_count = Some(nodes);
            facts.edge_count = Some(edges);
            facts.counts_exact = true;
        }
        Err(_) => {
            facts.node_count = estimated_nodes(&conn);
            facts.edge_count = estimated_edges(&conn);
            facts.counts_exact = false;
        }
    }
    Ok(facts)
}

fn languages(conn: &Connection) -> rusqlite::Result<Vec<LanguageCount>> {
    let mut stmt = conn.prepare(
        "SELECT language, COUNT(*) AS n FROM files GROUP BY language ORDER BY n DESC, language",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(LanguageCount {
            language: r.get(0)?,
            files: r.get::<_, i64>(1)?.max(0).unsigned_abs(),
        })
    })?;
    rows.collect()
}

/// `MAX(files.indexed_at)`, lenient about REAL timestamps (TS-written DBs).
fn last_indexed(conn: &Connection) -> Option<i64> {
    conn.query_row("SELECT MAX(indexed_at) FROM files", [], |r| {
        use rusqlite::types::ValueRef;
        Ok(match r.get_ref(0)? {
            ValueRef::Integer(i) => Some(i),
            #[allow(clippy::cast_possible_truncation)]
            ValueRef::Real(f) => Some(f as i64),
            _ => None,
        })
    })
    .ok()
    .flatten()
}

fn indexed_manifests(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT path FROM files
         WHERE path IN ('Cargo.toml', 'package.json', 'go.mod')
            OR path LIKE '%/Cargo.toml' OR path LIKE '%/package.json' OR path LIKE '%/go.mod'
         ORDER BY path LIMIT ?1",
    )?;
    let rows = stmt.query_map([MAX_INDEXED_MANIFESTS as i64], |r| r.get(0))?;
    rows.collect()
}

fn exact_counts(conn: &Connection) -> rusqlite::Result<(u64, u64)> {
    let count = |table: &str| -> rusqlite::Result<u64> {
        let n: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?;
        Ok(n.max(0).unsigned_abs())
    };
    Ok((count("nodes")?, count("edges")?))
}

/// `edges.id` is `AUTOINCREMENT` and a full re-index clears the table, so
/// the id span is the row count less whatever syncs deleted since — two
/// index seeks. `sqlite_stat1` when the span is unavailable.
fn estimated_edges(conn: &Connection) -> Option<u64> {
    conn.query_row("SELECT MAX(id) - MIN(id) + 1 FROM edges", [], |r| {
        r.get::<_, Option<i64>>(0)
    })
    .ok()
    .flatten()
    .map(|n| n.max(0).unsigned_abs())
    .or_else(|| stat1_rows(conn, "edges"))
}

fn estimated_nodes(conn: &Connection) -> Option<u64> {
    conn.query_row("SELECT SUM(node_count) FROM files", [], |r| {
        r.get::<_, Option<i64>>(0)
    })
    .ok()
    .flatten()
    .map(|n| n.max(0).unsigned_abs())
    .or_else(|| stat1_rows(conn, "nodes"))
}

/// Row estimate `ANALYZE` left in `sqlite_stat1` for `table`.
fn stat1_rows(conn: &Connection, table: &str) -> Option<u64> {
    let stat: String = conn
        .query_row(
            "SELECT stat FROM sqlite_stat1 WHERE tbl = ?1 ORDER BY idx IS NULL DESC LIMIT 1",
            [table],
            |r| r.get(0),
        )
        .ok()?;
    stat.split_whitespace().next()?.parse().ok()
}

/// Run `f`, interrupting whatever statement it is in once `budget` passes.
///
/// An interrupt with no statement running is a no-op in SQLite, so past
/// the deadline the watchdog keeps interrupting until `f` returns (it is
/// joined before this returns, so it never touches a later statement).
fn with_deadline<T>(
    conn: &Connection,
    budget: Duration,
    f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
) -> rusqlite::Result<T> {
    const RETRY: Duration = Duration::from_millis(5);
    let handle = conn.get_interrupt_handle();
    let (done, finished) = mpsc::channel::<()>();
    let deadline = Instant::now() + budget;
    std::thread::scope(|scope| {
        scope.spawn(move || {
            loop {
                let wait = deadline.saturating_duration_since(Instant::now());
                match finished.recv_timeout(if wait.is_zero() { RETRY } else { wait }) {
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if Instant::now() >= deadline {
                            handle.interrupt();
                        }
                    }
                    _ => return,
                }
            }
        });
        let out = f(conn);
        drop(done);
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_budget_falls_back_to_estimates() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files (path TEXT, language TEXT, node_count INTEGER);
             CREATE TABLE nodes (id TEXT);
             CREATE TABLE edges (id INTEGER);
             INSERT INTO files VALUES ('a.rs', 'rust', 3), ('b.rs', 'rust', 4);
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 200000)
             INSERT INTO edges SELECT i FROM n;
             ANALYZE;",
        )
        .unwrap();
        let exact = with_deadline(&conn, Duration::from_secs(30), exact_counts).unwrap();
        assert_eq!(exact, (0, 200_000));
        assert!(
            with_deadline(&conn, Duration::ZERO, |c| {
                // A statement that outlives any budget.
                c.query_row(
                    "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n)
                 SELECT COUNT(*) FROM n",
                    [],
                    |r| r.get::<_, i64>(0),
                )
            })
            .is_err()
        );
        assert_eq!(estimated_nodes(&conn), Some(7));
        assert_eq!(stat1_rows(&conn, "edges"), Some(200_000));
        assert_eq!(estimated_edges(&conn), Some(200_000));
        // The connection is usable after an interrupt.
        assert_eq!(languages(&conn).unwrap()[0].files, 2);
    }
}

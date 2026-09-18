//! Incremental ingest into the cross-session memory: every [`EventSource`]
//! resumes from its per-unit checkpoints, within a per-run budget, one
//! transaction per batch (checkpoints commit with their events), then the
//! per-repository rollups.
//!
//! Only `codegraph history ingest` runs this — detached (see
//! [`super::background`]) when a time-boxed caller (the prompt hook, an MCP
//! request) wants fresher history, never inline in one.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::params;

use super::memory::{MemoryWriter, WriteStats, roll_up};
use super::sources::{Budget, EventSource, SourceEvent, SourceStats, Visit};
use super::store::{HistoryDb, HistoryError};
use super::time::now_ms;
use crate::utils::FileLock;

/// Events per transaction (committed at the next unit boundary).
const BATCH: usize = 5_000;

/// Knobs for an incremental pass.
#[derive(Debug, Clone)]
pub struct IncrementalOptions {
    /// Wall-clock budget for reading sources (`None`: unlimited).
    pub time_budget: Option<Duration>,
    /// Tool calls per run (`None`: unlimited).
    pub max_events: Option<usize>,
    /// Only sessions that worked under this directory.
    pub scope: Option<PathBuf>,
    /// Attribute every call's `tool_events.project` to this path.
    pub project: Option<String>,
    /// Mine git co-change for the repositories touched.
    pub git: bool,
}

impl Default for IncrementalOptions {
    /// The detached run's budget: 5 s or 50k calls, whichever first.
    fn default() -> Self {
        Self {
            time_budget: Some(Duration::from_secs(5)),
            max_events: Some(50_000),
            scope: None,
            project: None,
            git: true,
        }
    }
}

/// What one source contributed.
#[derive(Debug, Clone, Default)]
pub struct SourceReport {
    pub source: &'static str,
    pub location: String,
    pub stats: SourceStats,
    pub writes: WriteStats,
}

/// What an incremental pass did.
#[derive(Debug, Clone, Default)]
pub struct IncrementalReport {
    pub sources: Vec<SourceReport>,
    /// Repositories whose rollups were rebuilt.
    pub repos: usize,
    /// Changed input units left for the next run.
    pub deferred: usize,
}

impl HistoryDb {
    /// Read every source from its checkpoints into the memory, then rebuild
    /// the rollups of the repositories that changed.
    pub fn ingest_events(
        &mut self,
        sources: &[&dyn EventSource],
        opts: &IncrementalOptions,
    ) -> Result<IncrementalReport, HistoryError> {
        let budget = Budget::new(opts.time_budget, opts.max_events);
        let mut report = IncrementalReport::default();
        let mut repos: HashSet<i64> = HashSet::new();
        for source in sources {
            let (source_report, touched) = self.ingest_one(*source, &budget, opts)?;
            report.deferred += source_report.stats.deferred;
            report.sources.push(source_report);
            repos.extend(touched);
        }
        let now = now_ms();
        let conn = self.conn();
        let mut repos: Vec<i64> = repos.into_iter().collect();
        repos.sort_unstable();
        for repo in &repos {
            roll_up(conn, *repo, opts.git, now)?;
        }
        report.repos = repos.len();
        conn.execute(
            "INSERT OR REPLACE INTO ingest_state (source, key, value) VALUES ('_meta', 'last_run', ?1)",
            params![now.to_string()],
        )?;
        Ok(report)
    }

    fn ingest_one(
        &mut self,
        source: &dyn EventSource,
        budget: &Budget,
        opts: &IncrementalOptions,
    ) -> Result<(SourceReport, HashSet<i64>), HistoryError> {
        let conn = self.conn();
        let cursors = load_cursors(conn, source.id())?;
        let visit = Visit {
            cursors: &cursors,
            budget,
            scope: opts.scope.as_deref(),
        };
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let mut writer = MemoryWriter::new(conn, source.id(), opts.project.clone());
        let mut since_commit = 0usize;
        let visited = source.visit_events(&visit, &mut |event| {
            let checkpoint = matches!(event, SourceEvent::Checkpoint { .. });
            if matches!(event, SourceEvent::Call(_)) {
                budget.spend();
            }
            writer.write(event)?;
            since_commit += 1;
            if checkpoint && since_commit >= BATCH {
                conn.execute_batch("COMMIT; BEGIN IMMEDIATE")?;
                since_commit = 0;
            }
            Ok(())
        });
        let stats = match visited.and_then(|stats| writer.finish().map(|()| stats)) {
            Ok(stats) => {
                conn.execute_batch("COMMIT")?;
                stats
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(e);
            }
        };
        let touched = writer.touched_repos().clone();
        Ok((
            SourceReport {
                source: source.id(),
                location: source.location(),
                stats,
                writes: writer.stats,
            },
            touched,
        ))
    }
}

fn load_cursors(
    conn: &rusqlite::Connection,
    source: &str,
) -> rusqlite::Result<HashMap<String, String>> {
    let mut stmt = conn.prepare("SELECT key, value FROM ingest_state WHERE source = ?1")?;
    let rows = stmt.query_map(params![source], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

/// The single-writer lock of the store at `db_path` (`<db>.lock`).
pub fn writer_lock(db_path: &Path) -> FileLock {
    let mut name = db_path.as_os_str().to_owned();
    name.push(".lock");
    FileLock::new(PathBuf::from(name))
}

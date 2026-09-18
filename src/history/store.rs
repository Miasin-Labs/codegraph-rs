//! [`HistoryDb`]: the SQLite store behind `codegraph history`.
//!
//! Writers ([`HistoryDb::open`]) create the file owner-only (0600, its
//! directory 0700) and migrate it; readers ([`HistoryDb::open_read_only`])
//! never create or change anything and report "no history yet" as `None`.

use std::path::Path;
use std::{fs, io};

use rusqlite::{Connection, Statement, named_params, params};

use super::event::{RawToolCall, ToolEvent};
use super::project::ProjectResolver;
use super::schema;
use super::sources::{SourceStats, ToolCallSource};

/// Errors from the history store and its source adapters.
#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("history database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("history I/O: {0}")]
    Io(#[from] io::Error),
    /// Reading the atlas (linked projects) failed.
    #[error("{0}")]
    Atlas(#[from] crate::atlas::AtlasError),
}

/// Knobs for an ingest pass.
#[derive(Debug, Clone, Default)]
pub struct IngestOptions {
    /// Attribute every ingested call to this project instead of deriving it.
    pub project: Option<String>,
}

/// What an ingest pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IngestReport {
    /// What the adapter read and emitted.
    pub source: SourceStats,
    /// New rows written.
    pub inserted: usize,
    /// Calls already stored by an earlier pass (same call key).
    pub already_present: usize,
    /// Calls with at least one masked value.
    pub redacted: usize,
}

const INSERT_SQL: &str = "INSERT OR IGNORE INTO tool_events \
     (call_key, source, ts, session, project, tool_kind, primary_cmd, chain, path, redacted) \
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)";

/// Optional project filter shared by the ranking queries: a plain substring
/// match (`instr`, so `_`/`%` in the filter are literal).
const IN_SCOPE: &str = "(:project IS NULL OR instr(project, :project) > 0)";

/// SQLite-backed history store.
pub struct HistoryDb {
    conn: Connection,
}

impl HistoryDb {
    /// Open for writing, creating the file (0600) and its directory (0700)
    /// when absent, and migrating it to the current schema.
    pub fn open(path: &Path) -> Result<Self, HistoryError> {
        create_private(path)?;
        let conn = Connection::open(path)?;
        // WAL: the prompt hook and MCP readers never wait on an ingest.
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Self::init(conn)
    }

    /// The underlying connection (the memory writer and rollups).
    pub(super) fn conn(&self) -> &Connection {
        &self.conn
    }

    /// In-memory store (tests).
    pub fn open_in_memory() -> Result<Self, HistoryError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self, HistoryError> {
        // Deleted rows (a migration purge) get their pages zeroed, not left in free space.
        conn.pragma_update(None, "secure_delete", true)?;
        schema::migrate(&mut conn)?;
        Ok(Self { conn })
    }

    /// Open an existing store read-only. `Ok(None)` when there is no history
    /// yet (no file, or no table); never creates (not even SQLite's side
    /// files) or migrates anything.
    pub fn open_read_only(path: &Path) -> Result<Option<Self>, HistoryError> {
        if !path.is_file() {
            return Ok(None);
        }
        let conn = crate::atlas::ro::open_read_only(path, std::time::Duration::from_millis(500))?;
        let has_table: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'tool_events')",
            [],
            |r| r.get(0),
        )?;
        Ok(has_table.then_some(Self { conn }))
    }

    /// The store's schema version (`PRAGMA user_version`).
    pub fn schema_version(&self) -> Result<i64, HistoryError> {
        Ok(schema::user_version(&self.conn)?)
    }

    /// Ingest every call `source` yields, in one transaction. Idempotent:
    /// a call already stored (same source + native id) is skipped.
    pub fn ingest_source(
        &mut self,
        source: &dyn ToolCallSource,
        opts: &IngestOptions,
    ) -> Result<IngestReport, HistoryError> {
        let tx = self.conn.transaction()?;
        let mut writer = Writer::new(&tx, source.id(), opts)?;
        let stats = source.visit(&mut |raw| writer.write(raw))?;
        let mut report = writer.finish();
        tx.commit()?;
        report.source = stats;
        Ok(report)
    }

    /// Ingest already-collected calls from source `source_id` (same
    /// semantics as [`Self::ingest_source`]).
    pub fn ingest_calls(
        &mut self,
        source_id: &str,
        calls: impl IntoIterator<Item = RawToolCall>,
        opts: &IngestOptions,
    ) -> Result<IngestReport, HistoryError> {
        let tx = self.conn.transaction()?;
        let mut writer = Writer::new(&tx, source_id, opts)?;
        for raw in calls {
            writer.report.source.calls += 1;
            writer.write(raw)?;
        }
        let report = writer.finish();
        tx.commit()?;
        Ok(report)
    }

    /// Rows in scope (optionally a project path substring).
    pub fn count(&self, project: Option<&str>) -> Result<i64, HistoryError> {
        let sql = format!("SELECT COUNT(*) FROM tool_events WHERE {IN_SCOPE}");
        Ok(self
            .conn
            .query_row(&sql, named_params! {":project": project}, |r| r.get(0))?)
    }

    /// Tool kinds by frequency.
    pub fn hot_tools(
        &self,
        project: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, i64)>, HistoryError> {
        self.grouped(
            &format!(
                "SELECT tool_kind, COUNT(*) c FROM tool_events WHERE {IN_SCOPE} \
                 GROUP BY tool_kind ORDER BY c DESC, tool_kind LIMIT :limit"
            ),
            project,
            limit,
        )
    }

    /// Most-touched file paths.
    pub fn hot_files(
        &self,
        project: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, i64)>, HistoryError> {
        self.grouped(
            &format!(
                "SELECT path, COUNT(*) c FROM tool_events WHERE path IS NOT NULL AND {IN_SCOPE} \
                 GROUP BY path ORDER BY c DESC, path LIMIT :limit"
            ),
            project,
            limit,
        )
    }

    /// Most common normalized command chains.
    pub fn hot_chains(
        &self,
        project: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, i64)>, HistoryError> {
        self.grouped(
            &format!(
                "SELECT chain, COUNT(*) c FROM tool_events \
                 WHERE chain IS NOT NULL AND chain <> '' AND {IN_SCOPE} \
                 GROUP BY chain ORDER BY c DESC, chain LIMIT :limit"
            ),
            project,
            limit,
        )
    }

    /// Most common primary commands (the command a shell call was for).
    pub fn hot_commands(
        &self,
        project: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, i64)>, HistoryError> {
        self.grouped(
            &format!(
                "SELECT primary_cmd, COUNT(*) c FROM tool_events \
                 WHERE primary_cmd IS NOT NULL AND {IN_SCOPE} \
                 GROUP BY primary_cmd ORDER BY c DESC, primary_cmd LIMIT :limit"
            ),
            project,
            limit,
        )
    }

    /// File pairs read/edited in the same session — co-access coupling the
    /// static call graph misses. Requires at least 2 co-occurrences.
    ///
    /// Bounded for scale: a naive all-pairs self-join explodes on large
    /// sessions (quadratic in session size). We first reduce to the globally
    /// hottest files and the *distinct* (session, path) pairs among them, so
    /// the join is over a small, fixed candidate set regardless of corpus size.
    pub fn co_access(
        &self,
        project: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, String, i64)>, HistoryError> {
        const HOT_POOL: i64 = 300;
        let sql = format!(
            "WITH hot AS ( \
                 SELECT path FROM tool_events \
                 WHERE path IS NOT NULL AND {IN_SCOPE} \
                 GROUP BY path ORDER BY COUNT(*) DESC, path LIMIT :pool \
             ), ev AS ( \
                 SELECT DISTINCT session, path FROM tool_events \
                 WHERE session IS NOT NULL AND path IS NOT NULL AND {IN_SCOPE} \
                   AND path IN (SELECT path FROM hot) \
             ) \
             SELECT a.path, b.path, COUNT(*) c \
             FROM ev a JOIN ev b ON a.session = b.session AND a.path < b.path \
             GROUP BY a.path, b.path HAVING c >= 2 \
             ORDER BY c DESC, a.path LIMIT :limit"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let args = named_params! {":limit": limit as i64, ":project": project, ":pool": HOT_POOL};
        let rows = stmt.query_map(args, |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn grouped(
        &self,
        sql: &str,
        project: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, i64)>, HistoryError> {
        let mut stmt = self.conn.prepare(sql)?;
        let args = named_params! {":limit": limit as i64, ":project": project};
        let rows = stmt.query_map(args, |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

/// Builds rows from raw calls (redacting on the way) and inserts them.
struct Writer<'c> {
    stmt: Statement<'c>,
    source_id: String,
    project: Option<String>,
    projects: ProjectResolver,
    report: IngestReport,
}

impl<'c> Writer<'c> {
    fn new(conn: &'c Connection, source_id: &str, opts: &IngestOptions) -> rusqlite::Result<Self> {
        Ok(Self {
            stmt: conn.prepare(INSERT_SQL)?,
            source_id: source_id.to_owned(),
            project: opts.project.clone(),
            projects: ProjectResolver::new(),
            report: IngestReport::default(),
        })
    }

    fn write(&mut self, mut raw: RawToolCall) -> Result<(), HistoryError> {
        if self.project.is_some() {
            raw.project.clone_from(&self.project);
        }
        let ev = ToolEvent::from_raw(&self.source_id, &raw, &mut self.projects);
        let changed = self.stmt.execute(params![
            ev.call_key,
            ev.source,
            ev.ts,
            ev.session,
            ev.project,
            ev.tool_kind,
            ev.primary_cmd,
            ev.chain,
            ev.path,
            i64::from(ev.redacted),
        ])?;
        if changed > 0 {
            self.report.inserted += 1;
        } else {
            self.report.already_present += 1;
        }
        self.report.redacted += usize::from(ev.redacted);
        Ok(())
    }

    fn finish(self) -> IngestReport {
        self.report
    }
}

/// Create the missing parent directories of the store at `path` owner-only
/// (0700), so its lock file can be taken before the store is opened.
pub fn ensure_store_dir(path: &Path) -> io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        if !dir.exists() {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(dir)?;
        }
    }
    Ok(())
}

/// Create `path` owner-only (0600) — and its missing parent directories
/// 0700 — if it doesn't exist yet. Existing files and directories are left
/// as they are.
fn create_private(path: &Path) -> io::Result<()> {
    ensure_store_dir(path)?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests;

//! Build one shard: index a dependency's library sources into
//! `deps/<ecosystem>/<name>-<version>/`.
//!
//! * **Read-only source.** The index is detached ([`CodeGraph::init_detached`]):
//!   database and lock live in the shard, never in the source tree.
//! * **Atomic.** Everything is built in a hidden `.tmp-` sibling, compacted
//!   to a single self-contained database file, described by `meta.json`,
//!   and only then renamed into place. Readers see the old shard or the new
//!   one, never a half-built one; a killed build leaves only a `.tmp-`
//!   directory for `gc`.
//! * **Exclusive.** One OS lock per shard: a second builder (another
//!   process, another project's background build) skips it.
//! * **Idempotent.** A shard built from the same source by the current
//!   extractor and schema, under budgets at least as large, is left alone.
//! * **Bounded.** File/byte budgets choose the files ([`crate::deps::scope`]);
//!   wall-clock and database-size budgets stop extraction between batches.
//!   Any of them makes the shard `partial`, never a failure.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::meta::{META_FORMAT, ShardCounts, ShardMeta};
use crate::IndexOptions;
use crate::codegraph::CodeGraph;
use crate::db::{CURRENT_SCHEMA_VERSION, DATABASE_FILENAME};
use crate::deps::model::{DepKey, DepSource, ShardState};
use crate::deps::scope::{self, PartialReason, Selection, ShardLimits};
use crate::deps::store::{DepsHome, OLD_PREFIX, StoreLock, TMP_PREFIX};
use crate::extraction::EXTRACTION_VERSION;

/// One shard to build.
#[derive(Debug, Clone)]
pub struct BuildRequest<'a> {
    pub key: &'a DepKey,
    pub source: &'a DepSource,
    /// The located source tree (read, never written).
    pub source_dir: &'a Path,
    pub limits: ShardLimits,
    /// Rebuild even when the existing shard is up to date.
    pub force: bool,
}

/// What a build did.
#[derive(Debug, Clone)]
pub enum BuildOutcome {
    /// A new shard was published.
    Built(ShardMeta),
    /// The existing shard already matches the source, extractor and budgets.
    UpToDate(ShardMeta),
    /// Another builder holds this shard's lock.
    Locked,
    /// The source holds no library files to index (e.g. an npm package
    /// that ships only a native binary).
    NoSources,
    /// The build failed; nothing was published.
    Failed(String),
}

impl BuildOutcome {
    pub fn meta(&self) -> Option<&ShardMeta> {
        match self {
            Self::Built(meta) | Self::UpToDate(meta) => Some(meta),
            Self::Locked | Self::NoSources | Self::Failed(_) => None,
        }
    }
}

/// Build (or confirm) the shard `request` describes.
pub async fn build_shard(home: &DepsHome, request: &BuildRequest<'_>) -> BuildOutcome {
    let key = request.key;
    let ecosystem_dir = home.ecosystem_dir(key.ecosystem);
    if let Err(error) = home
        .ensure()
        .and_then(|()| fs::create_dir_all(&ecosystem_dir))
    {
        return BuildOutcome::Failed(format!("create {}: {error}", ecosystem_dir.display()));
    }
    let _lock = match StoreLock::try_acquire(&home.shard_lock_path(key)) {
        Ok(Some(lock)) => lock,
        Ok(None) => return BuildOutcome::Locked,
        Err(error) => return BuildOutcome::Failed(format!("lock: {error}")),
    };

    let selection = scope::select(key.ecosystem, request.source_dir, &request.limits);
    let dest = home.shard_dir(key);
    if !request.force {
        if let Some(existing) = ShardMeta::read(&dest) {
            if is_up_to_date(&existing, key, &selection, &request.limits) {
                return BuildOutcome::UpToDate(existing);
            }
        }
    }
    if selection.files.is_empty() {
        return BuildOutcome::NoSources;
    }

    let tmp = ecosystem_dir.join(format!(
        "{TMP_PREFIX}{}-{}-{}",
        key.dir_name(),
        std::process::id(),
        unique_suffix()
    ));
    let result = build_into(&tmp, request, &selection).await;
    let meta = match result {
        Ok(meta) => meta,
        Err(error) => {
            let _ = fs::remove_dir_all(&tmp);
            return BuildOutcome::Failed(error);
        }
    };
    match publish(&tmp, &dest) {
        Ok(()) => BuildOutcome::Built(meta),
        Err(error) => {
            let _ = fs::remove_dir_all(&tmp);
            BuildOutcome::Failed(format!("publish {}: {error}", dest.display()))
        }
    }
}

/// The existing shard needs no rebuild: same key and source, current
/// extractor and schema, and — if it was cut short — budgets no smaller
/// than the ones asked for now.
fn is_up_to_date(
    existing: &ShardMeta,
    key: &DepKey,
    selection: &Selection,
    limits: &ShardLimits,
) -> bool {
    existing.key() == *key
        && existing.is_current()
        && existing.is_readable()
        && existing.source_fingerprint == selection.fingerprint
        && (existing.partial_reasons.is_empty() || existing.limits.covers(limits))
}

async fn build_into(
    tmp: &Path,
    request: &BuildRequest<'_>,
    selection: &Selection,
) -> Result<ShardMeta, String> {
    let started = Instant::now();
    let cg = CodeGraph::init_detached(request.source_dir, tmp)
        .map_err(|e| format!("create index: {e}"))?;
    let db_path = tmp.join(DATABASE_FILENAME);
    let watch = BudgetWatch::start(
        Duration::from_millis(request.limits.time_ms.max(1)),
        db_path.clone(),
        request.limits.max_db_bytes,
    );
    let indexed = cg
        .index_file_list(
            &selection.files,
            &IndexOptions {
                signal: Some(watch.flag()),
                ..IndexOptions::default()
            },
        )
        .await;
    let stopped_by = watch.finish();
    let stats = cg.get_stats();
    // Every query handle shares the connection; it closes with the graph.
    cg.close();
    drop(cg);
    let indexed = indexed.map_err(|e| format!("index: {e}"))?;
    if !indexed.success {
        let reason = indexed
            .errors
            .iter()
            .find(|e| e.severity == crate::types::Severity::Error)
            .map_or_else(|| "index failed".to_string(), |e| e.message.clone());
        return Err(reason);
    }
    let stats = stats.map_err(|e| format!("stats: {e}"))?;

    compact(&db_path).map_err(|e| format!("compact: {e}"))?;
    let _ = fs::remove_file(tmp.join("codegraph.lock"));

    let accounted = indexed.files_indexed + indexed.files_skipped + indexed.files_errored;
    let mut partial_reasons = selection.truncated_by.clone();
    if accounted < selection.files.len() {
        partial_reasons.push(stopped_by.unwrap_or(PartialReason::Time));
    }
    let meta = ShardMeta {
        format: META_FORMAT,
        ecosystem: request.key.ecosystem,
        name: request.key.name.clone(),
        version: request.key.version.clone(),
        source: request.source.clone(),
        source_dir: request.source_dir.to_string_lossy().into_owned(),
        source_fingerprint: selection.fingerprint.clone(),
        source_mtime_ms: selection.newest_mtime_ms,
        extractor_version: EXTRACTION_VERSION,
        schema_version: CURRENT_SCHEMA_VERSION,
        codegraph_version: env!("CARGO_PKG_VERSION").to_string(),
        built_at_ms: now_ms(),
        build_ms: started.elapsed().as_millis() as u64,
        state: if partial_reasons.is_empty() {
            ShardState::Ready
        } else {
            ShardState::Partial
        },
        partial_reasons,
        limits: request.limits,
        counts: ShardCounts {
            candidate_files: selection.candidates,
            candidate_bytes: selection.candidate_bytes,
            selected_files: selection.files.len(),
            selected_bytes: selection.bytes,
            indexed_files: indexed.files_indexed,
            errored_files: indexed.files_errored,
            oversized_files: selection.oversized,
            nodes: stats.node_count,
            edges: stats.edge_count,
        },
        db_bytes: fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0),
    };
    meta.write(tmp).map_err(|e| format!("write meta: {e}"))?;
    Ok(meta)
}

/// Fold the WAL into one self-contained, rollback-journal database file:
/// what the immutable read-only open ([`super::handle`]) requires.
fn compact(db_path: &Path) -> rusqlite::Result<()> {
    let conn = rusqlite::Connection::open(db_path)?;
    conn.execute_batch("PRAGMA optimize; VACUUM;")?;
    conn.query_row("PRAGMA journal_mode = DELETE", [], |_| Ok(()))?;
    conn.close().map_err(|(_, e)| e)
}

/// Move `tmp` into place at `dest`, replacing any previous shard. The old
/// shard is renamed aside first and deleted after; a failed swap restores it.
fn publish(tmp: &Path, dest: &Path) -> std::io::Result<()> {
    if !dest.exists() {
        return fs::rename(tmp, dest);
    }
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let old: PathBuf = dest.with_file_name(format!(
        "{OLD_PREFIX}{name}-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    fs::rename(dest, &old)?;
    if let Err(error) = fs::rename(tmp, dest) {
        let _ = fs::rename(&old, dest);
        return Err(error);
    }
    let _ = fs::remove_dir_all(&old);
    Ok(())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
}

/// How often the watcher samples the clock and the database size.
const WATCH_INTERVAL: Duration = Duration::from_millis(100);

const FIRED_NONE: u8 = 0;
const FIRED_TIME: u8 = 1;
const FIRED_DB_SIZE: u8 = 2;

/// Raises the extraction stop flag once the time budget elapses or the
/// database (with its WAL) outgrows its budget; [`BudgetWatch::finish`]
/// stops the watcher and says which budget, if any, fired.
struct BudgetWatch {
    flag: Arc<AtomicBool>,
    reason: Arc<AtomicU8>,
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl BudgetWatch {
    fn start(budget: Duration, db_path: PathBuf, max_db_bytes: u64) -> Self {
        let flag = Arc::new(AtomicBool::new(false));
        let reason = Arc::new(AtomicU8::new(FIRED_NONE));
        let (stop, stopped) = mpsc::channel::<()>();
        let (thread_flag, thread_reason) = (Arc::clone(&flag), Arc::clone(&reason));
        let started = Instant::now();
        let thread = std::thread::Builder::new()
            .name("codegraph-shard-budget".into())
            .spawn(move || {
                loop {
                    let left = budget.saturating_sub(started.elapsed());
                    if !matches!(
                        stopped.recv_timeout(left.min(WATCH_INTERVAL)),
                        Err(mpsc::RecvTimeoutError::Timeout)
                    ) {
                        return; // finished (or dropped) before any budget ran out
                    }
                    let fired = if started.elapsed() >= budget {
                        FIRED_TIME
                    } else if database_bytes(&db_path) > max_db_bytes {
                        FIRED_DB_SIZE
                    } else {
                        continue;
                    };
                    thread_reason.store(fired, Ordering::Relaxed);
                    thread_flag.store(true, Ordering::Relaxed);
                    return;
                }
            })
            .ok();
        Self {
            flag,
            reason,
            stop: Some(stop),
            thread,
        }
    }

    fn flag(&self) -> &AtomicBool {
        &self.flag
    }

    /// Stop watching; the budget that stopped extraction, if one did.
    fn finish(mut self) -> Option<PartialReason> {
        self.shutdown();
        match self.reason.load(Ordering::Relaxed) {
            FIRED_TIME => Some(PartialReason::Time),
            FIRED_DB_SIZE => Some(PartialReason::DbSize),
            _ => None,
        }
    }

    fn shutdown(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for BudgetWatch {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The database file plus its write-ahead log.
fn database_bytes(db_path: &Path) -> u64 {
    let mut wal = db_path.as_os_str().to_owned();
    wal.push("-wal");
    [db_path.to_path_buf(), PathBuf::from(wal)]
        .iter()
        .filter_map(|p| fs::metadata(p).ok())
        .map(|m| m.len())
        .sum()
}

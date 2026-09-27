//! Staging and indexing units the way `tools/bugbench` does — a fresh copy
//! of each unit (the corpora are never indexed in place), indexed by
//! `codegraph init` under a scratch `CODEGRAPH_HOME` with the atlas,
//! dependency graphs, external resolution, history and background work off
//! — plus what bugbench does not: an indexed copy is kept with a stamp of
//! its sources and the extractor, and reused while both are unchanged, so
//! scoring another rule costs no re-index. Findings are cached per unit
//! under the rules' hash, which is what makes a deadline-cut run resumable.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::ScoredFinding;
use super::corpus::Unit;
use crate::utils::sha256_hex;

/// Environment of every indexing child: nothing that registers, builds
/// dependency shards, spawns background work or reaches into other graphs
/// (bugbench's `ISOLATION_ENV`).
const ISOLATION_ENV: &[(&str, &str)] = &[
    ("CODEGRAPH_ATLAS", "0"),
    ("CODEGRAPH_DEPS", "0"),
    ("CODEGRAPH_NO_BACKGROUND_SYNC", "1"),
    ("CODEGRAPH_EXTERNAL", "0"),
    ("CODEGRAPH_HISTORY", "0"),
    ("CODEGRAPH_RUST_DEPS", "0"),
    ("NO_COLOR", "1"),
];
/// Names never copied into a staged unit (bugbench's `IGNORE_ON_COPY`).
const IGNORE_ON_COPY: &[&str] = &[".codegraph", "ground_truth.jsonl", "results"];
/// How often a running index is polled for exit, deadline and cancel.
const POLL: Duration = Duration::from_millis(25);

/// Where one corpus's units are staged.
pub(crate) struct Workspace {
    /// `<work>/<corpus key>`: one directory per unit.
    pub root: PathBuf,
    /// Stamps, logs and findings caches.
    meta: PathBuf,
    /// The scratch `CODEGRAPH_HOME` of the indexing children.
    home: PathBuf,
    /// The corpus being staged (skipped if the work dir sits inside it).
    corpus: PathBuf,
    /// Each unit's source fingerprint, computed once per run.
    fingerprints: Mutex<HashMap<String, String>>,
}

/// `unit/name` → `unit__name` (bugbench's `safe_name`).
pub(crate) fn safe_name(unit: &str) -> String {
    unit.replace('/', "__")
}

/// Why a unit has no index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PrepareError {
    Stage(String),
    IndexFailed(String),
    IndexTimeout,
    /// The run's deadline (or a cancel) stopped it; it resumes next time.
    Interrupted,
}

impl PrepareError {
    pub fn status(&self) -> &'static str {
        match self {
            PrepareError::Stage(_) => "stage-failed",
            PrepareError::IndexFailed(_) => "index-failed",
            PrepareError::IndexTimeout => "index-timeout",
            PrepareError::Interrupted => "pending",
        }
    }
}

/// A unit's findings (and, for a pair's vulnerable side, its functions),
/// cached under the rules' hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct UnitCache {
    pub rules: String,
    pub findings: Vec<ScoredFinding>,
    /// `(ground-truth file, start line, end line)` of every function.
    #[serde(default)]
    pub functions: Vec<(String, i64, i64)>,
}

#[derive(Serialize, Deserialize, PartialEq)]
struct Stamp {
    sources: String,
    extractor: String,
}

impl Workspace {
    pub fn new(work: &Path, corpus: &Path) -> std::io::Result<Self> {
        let name = corpus
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "corpus".to_string());
        let key = &sha256_hex(corpus.to_string_lossy().as_bytes())[..8];
        let root = work.join(format!("{}-{key}", safe_name(&name)));
        let meta = root.join(".meta");
        std::fs::create_dir_all(&meta)?;
        let home = work.join("home");
        std::fs::create_dir_all(&home)?;
        Ok(Self {
            root,
            meta,
            home,
            corpus: corpus.to_path_buf(),
            fingerprints: Mutex::new(HashMap::new()),
        })
    }

    pub fn unit_dir(&self, unit: &Unit) -> PathBuf {
        self.root.join(safe_name(&unit.name))
    }

    fn meta_file(&self, unit: &Unit, suffix: &str) -> PathBuf {
        self.meta
            .join(format!("{}.{suffix}", safe_name(&unit.name)))
    }

    /// The cached findings of `unit` for the rules hashed `rules`.
    pub fn cached(&self, unit: &Unit, rules: &str) -> Option<UnitCache> {
        let text = std::fs::read_to_string(self.meta_file(unit, "findings.json")).ok()?;
        let cache: UnitCache = serde_json::from_str(&text).ok()?;
        (cache.rules == rules && self.is_fresh(unit)).then_some(cache)
    }

    pub fn store(&self, unit: &Unit, cache: &UnitCache) -> std::io::Result<()> {
        let path = self.meta_file(unit, "findings.json");
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(cache).unwrap_or_default())?;
        std::fs::rename(tmp, path)
    }

    fn stamp(&self, unit: &Unit) -> Stamp {
        Stamp {
            sources: self.fingerprint(unit),
            extractor: format!(
                "{}/{}",
                crate::extraction::EXTRACTION_VERSION,
                crate::db::migrations::CURRENT_SCHEMA_VERSION
            ),
        }
    }

    /// The unit's staged copy is indexed from its current sources by this
    /// extractor.
    pub fn is_fresh(&self, unit: &Unit) -> bool {
        let dir = self.unit_dir(unit);
        if !crate::directory::is_initialized(&dir) {
            return false;
        }
        let Ok(text) = std::fs::read_to_string(self.meta_file(unit, "stamp.json")) else {
            return false;
        };
        serde_json::from_str::<Stamp>(&text).is_ok_and(|stamp| stamp == self.stamp(unit))
    }

    /// Fingerprint `units` on up to `threads` threads (a stat of every
    /// source file: the one per-unit cost of a run that indexes nothing).
    pub fn warm(&self, units: &[Unit], threads: usize) {
        let chunk = units.len().div_ceil(threads.max(1)).max(1);
        std::thread::scope(|scope| {
            for part in units.chunks(chunk) {
                scope.spawn(move || {
                    for unit in part {
                        self.fingerprint(unit);
                    }
                });
            }
        });
    }

    /// (path, size, mtime) of every source file the unit copies, hashed;
    /// computed once per run.
    fn fingerprint(&self, unit: &Unit) -> String {
        if let Some(known) = self
            .fingerprints
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&unit.name)
        {
            return known.clone();
        }
        let fingerprint = self.compute_fingerprint(unit);
        self.fingerprints
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(unit.name.clone(), fingerprint.clone());
        fingerprint
    }

    fn compute_fingerprint(&self, unit: &Unit) -> String {
        let mut text = String::new();
        for rel in &unit.sources {
            let path = self.corpus.join(rel);
            for (file, meta) in self.source_files(&path) {
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_nanos());
                text.push_str(&format!("{}\0{}\0{mtime}\n", file.display(), meta.len()));
            }
        }
        sha256_hex(text.as_bytes())
    }

    /// The files under `path` a copy takes (or `path` itself).
    fn source_files(&self, path: &Path) -> Vec<(PathBuf, std::fs::Metadata)> {
        if !path.is_dir() {
            return std::fs::metadata(path)
                .map(|meta| vec![(path.to_path_buf(), meta)])
                .unwrap_or_default();
        }
        walkdir::WalkDir::new(path)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|entry| self.copied(entry.path(), entry.depth()))
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .filter_map(|entry| Some((entry.path().to_path_buf(), entry.metadata().ok()?)))
            .collect()
    }

    /// Whether a walked entry is copied: not an ignored name, not the work
    /// directory itself.
    fn copied(&self, path: &Path, depth: usize) -> bool {
        if depth == 0 {
            return true;
        }
        let ignored = path
            .file_name()
            .is_some_and(|name| IGNORE_ON_COPY.contains(&name.to_string_lossy().as_ref()));
        !ignored && !self.root.starts_with(path)
    }

    /// Copy `unit`'s sources into its directory (fresh inodes, never a
    /// hardlink: the RustSec trees hardlink identical files).
    fn stage(&self, unit: &Unit) -> Result<(), String> {
        let dest = self.unit_dir(unit);
        if dest.exists() {
            std::fs::remove_dir_all(&dest)
                .map_err(|e| format!("cannot clear {}: {e}", dest.display()))?;
        }
        let single_dir = unit.sources.len() == 1 && self.corpus.join(&unit.sources[0]).is_dir();
        if single_dir {
            let src = self.corpus.join(&unit.sources[0]);
            for entry in walkdir::WalkDir::new(&src)
                .sort_by_file_name()
                .into_iter()
                .filter_entry(|entry| self.copied(entry.path(), entry.depth()))
            {
                let entry = entry.map_err(|e| e.to_string())?;
                let rel = entry.path().strip_prefix(&src).unwrap_or(entry.path());
                let target = dest.join(rel);
                let kind = entry.file_type();
                if kind.is_dir() {
                    std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
                } else if kind.is_symlink() {
                    copy_symlink(entry.path(), &target)?;
                } else {
                    std::fs::copy(entry.path(), &target)
                        .map_err(|e| format!("{}: {e}", entry.path().display()))?;
                }
            }
        } else {
            for rel in &unit.sources {
                let target = dest.join(rel);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                std::fs::copy(self.corpus.join(rel), &target).map_err(|e| format!("{rel}: {e}"))?;
            }
        }
        Ok(())
    }

    /// Stage and index `unit` unless its indexed copy is fresh. Returns
    /// whether it indexed. `timeout` bounds one index and `cancel` kills
    /// it. The `deadline` never throws an index away: without `detach` an
    /// index already running finishes (it is stamped, and the next run
    /// scores it); with `detach` (a long-lived caller: the MCP server) it
    /// is left to finish on a watcher thread that stamps it, and this call
    /// returns at once.
    pub fn prepare(
        &self,
        unit: &Unit,
        binary: &Path,
        timeout: Duration,
        deadline: Option<Instant>,
        cancel: &AtomicBool,
        detach: bool,
    ) -> Result<bool, PrepareError> {
        let dest = self.unit_dir(unit);
        if in_background(&dest) {
            return Err(PrepareError::Interrupted);
        }
        if self.is_fresh(unit) {
            return Ok(false);
        }
        let stamp_path = self.meta_file(unit, "stamp.json");
        let _ = std::fs::remove_file(&stamp_path);
        self.stage(unit).map_err(PrepareError::Stage)?;
        let stamp = serde_json::to_vec(&self.stamp(unit)).unwrap_or_default();
        let log = self.meta_file(unit, "log");
        let log_file =
            std::fs::File::create(&log).map_err(|e| PrepareError::Stage(e.to_string()))?;
        let mut command = Command::new(binary);
        command
            .arg("init")
            .arg(&dest)
            .env("CODEGRAPH_HOME", &self.home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(log_file));
        for (key, value) in ISOLATION_ENV {
            command.env(key, value);
        }
        let mut child = command.spawn().map_err(|e| {
            PrepareError::IndexFailed(format!("cannot run {}: {e}", binary.display()))
        })?;
        let own_limit = Instant::now() + timeout;
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => break,
                Ok(Some(status)) => {
                    return Err(PrepareError::IndexFailed(format!(
                        "{status}: {}",
                        log_tail(&log)
                    )));
                }
                Ok(None) => {}
                Err(e) => return Err(PrepareError::IndexFailed(e.to_string())),
            }
            let now = Instant::now();
            if now >= own_limit {
                let _ = child.kill();
                let _ = child.wait();
                return Err(PrepareError::IndexTimeout);
            }
            if cancel.load(Ordering::SeqCst) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(PrepareError::Interrupted);
            }
            if detach && deadline.is_some_and(|d| now >= d) {
                finish_in_background(child, dest, own_limit, stamp_path, stamp);
                return Err(PrepareError::Interrupted);
            }
            std::thread::sleep(POLL);
        }
        std::fs::write(&stamp_path, stamp).map_err(|e| PrepareError::Stage(e.to_string()))?;
        Ok(true)
    }
}

/// Unit directories whose index a watcher thread is finishing.
static BACKGROUND: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Default::default);

fn in_background(dir: &Path) -> bool {
    BACKGROUND
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(dir)
}

/// Let `child` finish indexing `dir` (up to `limit`), then stamp it.
fn finish_in_background(
    mut child: Child,
    dir: PathBuf,
    limit: Instant,
    stamp_path: PathBuf,
    stamp: Vec<u8>,
) {
    BACKGROUND
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(dir.clone());
    std::thread::spawn(move || {
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if status.success() {
                        let _ = std::fs::write(&stamp_path, &stamp);
                    }
                    break;
                }
                Ok(None) if Instant::now() < limit => std::thread::sleep(POLL),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
            }
        }
        BACKGROUND
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&dir);
    });
}

/// The last lines of an index log.
fn log_tail(log: &Path) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let tail: Vec<char> = text.chars().rev().take(600).collect();
    tail.into_iter()
        .rev()
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(unix)]
fn copy_symlink(from: &Path, to: &Path) -> Result<(), String> {
    let target = std::fs::read_link(from).map_err(|e| e.to_string())?;
    std::os::unix::fs::symlink(target, to).map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn copy_symlink(from: &Path, to: &Path) -> Result<(), String> {
    if from.is_file() {
        std::fs::copy(from, to)
            .map(|_| ())
            .map_err(|e| e.to_string())
    } else {
        Ok(())
    }
}

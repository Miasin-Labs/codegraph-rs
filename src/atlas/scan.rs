//! Discovery: find existing CodeGraph indexes under some roots and register
//! them — read-only against the projects (never an index/sync), bounded by
//! depth and a time budget.
//!
//! The walk is iterative, never follows symlinks, does not descend into
//! dependency/build/VCS/cache directories ([`SKIP_DIRS`] — hidden
//! directories are otherwise walked: agent worktrees live under `.claude/`,
//! research clones under `.research/`), skips the machine-wide CodeGraph
//! directory, and keeps descending below a found index (indexes nest:
//! `repo/.codegraph` and `repo/research/x/.codegraph`).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::facts::{GatherOptions, ProjectFacts};
use super::kinds::ProjectStatus;
use super::store::{Atlas, AtlasError};
use crate::directory::{is_codegraph_data_dir, is_initialized};

/// Directory names never descended into: installed dependencies, build
/// output, VCS internals, tool caches and toolchains.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "__pycache__",
    "venv",
    "site-packages",
    "bower_components",
    "Pods",
    "DerivedData",
    ".git",
    ".hg",
    ".svn",
    ".jj",
    ".venv",
    ".tox",
    ".nox",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".cache",
    ".cargo",
    ".rustup",
    ".npm",
    ".pnpm-store",
    ".yarn",
    ".gradle",
    ".m2",
    ".next",
    ".nuxt",
    ".turbo",
    ".direnv",
    ".terraform",
    ".idea",
    ".vscode",
    ".Trash",
    ".local",
];
/// Absolute trees that are never projects.
const SKIP_ABSOLUTE: &[&str] = &["/proc", "/sys", "/dev", "/run"];
/// Per-project budget for exact node/edge counts during a scan.
const SCAN_COUNT_BUDGET: Duration = Duration::from_millis(250);

/// Knobs for a scan.
#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub roots: Vec<PathBuf>,
    /// Directory levels below each root to look at.
    pub max_depth: usize,
    /// Wall-clock budget for the walk and the registrations.
    pub budget: Duration,
    /// Trees to skip (the machine-wide CodeGraph directory).
    pub skip: Vec<PathBuf>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            max_depth: 8,
            budget: Duration::from_secs(120),
            skip: Vec::new(),
        }
    }
}

/// What a scan found and did.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    /// Index roots found, in walk order.
    pub found: Vec<PathBuf>,
    /// Registered for the first time.
    pub new: usize,
    /// Already known, refreshed.
    pub refreshed: usize,
    /// Found indexes by status.
    pub ok: usize,
    pub stale_schema: usize,
    pub unreadable: usize,
    /// Manifest links recorded.
    pub links: usize,
    /// Registrations skipped (contention or error), with why.
    pub skipped: Vec<(PathBuf, String)>,
    pub dirs_visited: usize,
    /// The walk stopped early at the time budget.
    pub budget_exhausted: bool,
    pub elapsed_ms: u64,
}

/// Walk `opts.roots`, registering every index found into `atlas`.
pub fn scan(atlas: &mut Atlas, opts: &ScanOptions) -> Result<ScanReport, AtlasError> {
    let started = Instant::now();
    let deadline = started + opts.budget;
    let mut report = ScanReport::default();
    let skip: Vec<PathBuf> = opts.skip.iter().map(|p| super::canonical_root(p)).collect();
    let mut on_found = |root: &Path, report: &mut ScanReport| {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let facts = ProjectFacts::gather(
            root,
            GatherOptions {
                count_budget: SCAN_COUNT_BUDGET.min(remaining),
            },
        );
        match facts.status {
            ProjectStatus::Ok => report.ok += 1,
            ProjectStatus::StaleSchema => report.stale_schema += 1,
            ProjectStatus::Unreadable | ProjectStatus::Missing => report.unreadable += 1,
        }
        match atlas.register_deferred(&facts) {
            Ok(done) => {
                report.links += done.links;
                if done.new {
                    report.new += 1;
                } else {
                    report.refreshed += 1;
                }
            }
            Err(e) => report.skipped.push((root.to_path_buf(), e.to_string())),
        }
    };
    find_indexes(opts, &skip, deadline, &mut report, &mut on_found);
    atlas.relink()?;
    report.elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(report)
}

/// The walk: calls `on_found` for each index root, in order.
fn find_indexes(
    opts: &ScanOptions,
    skip: &[PathBuf],
    deadline: Instant,
    report: &mut ScanReport,
    on_found: &mut dyn FnMut(&Path, &mut ScanReport),
) {
    let mut stack: Vec<(PathBuf, usize)> = opts
        .roots
        .iter()
        .rev()
        .map(|root| (super::canonical_root(root), 0))
        .collect();
    while let Some((dir, depth)) = stack.pop() {
        if Instant::now() >= deadline {
            report.budget_exhausted = true;
            return;
        }
        if skip.iter().any(|s| dir.starts_with(s))
            || SKIP_ABSOLUTE.iter().any(|s| dir.starts_with(s))
        {
            continue;
        }
        report.dirs_visited += 1;
        if is_initialized(&dir) && !report.found.contains(&dir) {
            report.found.push(dir.clone());
            on_found(&dir, report);
        }
        if depth >= opts.max_depth {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        let mut children: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter(|e| descend_into(&e.file_name().to_string_lossy()))
            .map(|e| e.path())
            .collect();
        children.sort();
        stack.extend(children.into_iter().rev().map(|child| (child, depth + 1)));
    }
}

fn descend_into(name: &str) -> bool {
    !is_codegraph_data_dir(name) && !SKIP_DIRS.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directory::get_codegraph_dir;

    /// A minimal index at `root`: the directory and a schema'd database.
    fn fake_index(root: &Path) {
        let dir = get_codegraph_dir(root);
        fs::create_dir_all(&dir).unwrap();
        let conn = rusqlite::Connection::open(dir.join("codegraph.db")).unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE schema_versions (version INTEGER PRIMARY KEY, applied_at INTEGER, description TEXT);
             INSERT INTO schema_versions VALUES ({}, 0, 'x');
             CREATE TABLE files (path TEXT PRIMARY KEY, language TEXT, indexed_at INTEGER, node_count INTEGER);
             INSERT INTO files VALUES ('src/lib.rs', 'rust', 1000, 2);
             CREATE TABLE nodes (id TEXT); CREATE TABLE edges (id INTEGER);
             CREATE TABLE project_metadata (key TEXT PRIMARY KEY, value TEXT, updated_at INTEGER);",
            crate::db::CURRENT_SCHEMA_VERSION
        ))
        .unwrap();
    }

    fn names(report: &ScanReport, base: &Path) -> Vec<String> {
        let base = super::super::canonical_root(base);
        report
            .found
            .iter()
            .map(|p| {
                p.strip_prefix(&base)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    #[test]
    fn scan_honours_depth_skips_and_nesting() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        fake_index(&base.join("a"));
        fake_index(&base.join("a/research/nested"));
        fake_index(&base.join("b/c/d/deep"));
        fake_index(&base.join("x/node_modules/pkg"));
        fake_index(&base.join("x/target/debug/build"));
        fake_index(&base.join("x/.cache/proj"));
        fake_index(&base.join("x/.git/proj"));
        fake_index(&base.join("x/.research/proj"));
        fake_index(&base.join("x/.claude/worktrees/agent"));
        fake_index(&base.join("home/proj"));

        let mut atlas = Atlas::open_in_memory().unwrap();
        let opts = ScanOptions {
            roots: vec![base.to_path_buf()],
            max_depth: 3,
            budget: Duration::from_secs(60),
            skip: vec![base.join("home")],
        };
        let report = scan(&mut atlas, &opts).unwrap();
        assert_eq!(
            names(&report, base),
            ["a", "a/research/nested", "x/.research/proj"]
        );
        assert!(!report.budget_exhausted);
        assert_eq!((report.new, report.ok), (3, 3));

        let deeper = ScanOptions {
            max_depth: 6,
            ..opts.clone()
        };
        let report = scan(&mut atlas, &deeper).unwrap();
        assert_eq!(
            names(&report, base),
            [
                "a",
                "a/research/nested",
                "b/c/d/deep",
                "x/.claude/worktrees/agent",
                "x/.research/proj"
            ]
        );
        assert_eq!((report.new, report.refreshed), (2, 3));
        let projects = atlas.projects().unwrap();
        assert_eq!(projects.len(), 5);
        // The nested index is linked to the project containing it.
        let parent = atlas.project_by_root(&base.join("a")).unwrap().unwrap();
        let nested = atlas.links_from(parent.id).unwrap();
        assert_eq!(nested.len(), 1);
        assert_eq!(nested[0].kind, super::super::LinkKind::NestedWorkspace);
        assert_eq!(nested[0].detail.as_deref(), Some("research/nested"));
    }

    #[test]
    fn an_exhausted_budget_stops_the_walk() {
        let tmp = tempfile::tempdir().unwrap();
        fake_index(&tmp.path().join("a"));
        let mut atlas = Atlas::open_in_memory().unwrap();
        let report = scan(
            &mut atlas,
            &ScanOptions {
                roots: vec![tmp.path().to_path_buf()],
                budget: Duration::ZERO,
                ..ScanOptions::default()
            },
        )
        .unwrap();
        assert!(report.budget_exhausted);
        assert!(report.found.is_empty());
        assert!(atlas.projects().unwrap().is_empty());
    }
}

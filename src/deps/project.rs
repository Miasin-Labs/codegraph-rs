//! Recording a project: parse its lockfiles, locate each dependency's
//! source, and store the result in the registry under the project's
//! canonical checkout root.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::error::DepsResult;
use super::locate::SourceRoots;
use super::lockfile::{self, Lockfile};
use super::model::{DepSource, Ecosystem};
use super::registry::{LocatedDep, RecordSummary, Registry};

/// A project's key everywhere in the federation (registry, atlas): its
/// canonical root path. Falls back to the path as given when it can't be
/// canonicalized (it no longer exists).
pub fn canonical_root(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Per-ecosystem tallies of one recording.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EcosystemCounts {
    pub dependencies: usize,
    pub direct: usize,
    /// Registry/git sources found on this machine.
    pub located: usize,
    /// Registry/git sources not found.
    pub unavailable: usize,
    /// Local path dependencies (the atlas's; no shard).
    pub path: usize,
}

/// What [`record_project`] did.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordReport {
    pub root: String,
    pub lockfiles: Vec<Lockfile>,
    /// The lockfiles were unchanged since the last recording; nothing was
    /// re-parsed.
    pub unchanged: bool,
    pub summary: RecordSummary,
    pub by_ecosystem: BTreeMap<Ecosystem, EcosystemCounts>,
    /// Lockfiles that failed to parse.
    pub errors: Vec<String>,
}

/// Parse, locate and record the dependencies of the project at `root`.
/// With `force` false, a project whose lockfiles are unchanged since it was
/// last recorded is left as is (`unchanged`). A project without lockfiles
/// is only written when it was recorded before (its usages are cleared).
pub fn record_project(
    registry: &mut Registry,
    root: &Path,
    roots: &SourceRoots,
    force: bool,
    now_ms: i64,
) -> DepsResult<RecordReport> {
    let canonical = canonical_root(root);
    let root_path = PathBuf::from(&canonical);
    let lockfiles = lockfile::discover(&root_path);
    let fingerprint = lockfile::fingerprint(&lockfiles);
    let previous = registry.project_fingerprint(&canonical)?;
    let mut report = RecordReport {
        root: canonical.clone(),
        lockfiles: lockfiles.clone(),
        ..RecordReport::default()
    };
    if lockfiles.is_empty() && previous.is_none() {
        return Ok(report);
    }
    if !force && previous.as_deref() == Some(fingerprint.as_str()) {
        report.unchanged = true;
        return Ok(report);
    }

    let manifest = lockfile::read_project(&root_path);
    report.errors = manifest.errors;
    let located: Vec<LocatedDep> = manifest
        .deps
        .into_iter()
        .map(|dep| {
            let source_dir = roots.locate(&dep, &root_path);
            LocatedDep { dep, source_dir }
        })
        .collect();
    for l in &located {
        let counts = report.by_ecosystem.entry(l.dep.key.ecosystem).or_default();
        counts.dependencies += 1;
        if l.dep.direct == Some(true) {
            counts.direct += 1;
        }
        match (&l.dep.source, &l.source_dir) {
            (DepSource::Path { .. }, _) => counts.path += 1,
            (_, Some(_)) => counts.located += 1,
            (_, None) => counts.unavailable += 1,
        }
    }
    report.summary =
        registry.record_project(&canonical, &fingerprint, lockfiles.len(), &located, now_ms)?;
    Ok(report)
}

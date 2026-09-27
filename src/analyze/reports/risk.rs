//! `codegraph analyze risk`: which functions a change touched, how far a
//! change to each reaches, and how many tests reach it — from git and the
//! call graph, no coverage run needed.
//!
//! The changed files are what `git diff <base>` reports (committed since the
//! base and uncommitted alike, relative to the project root) plus untracked
//! files. Every function and method the index holds in them is measured by
//! node id, so same-named definitions (`new`, `of`) stay apart: its
//! dependents (the impact radius, `contains` never climbed) and the test
//! functions among its dependents a few more hops out. A function no test
//! reaches that many dependents lean on is where a change is least guarded.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use serde::Serialize;

use crate::codegraph::CodeGraph;
use crate::search::{is_test_source_file, is_test_symbol};
use crate::types::{Node, NodeKind};

/// Default depth of the dependents counted as a function's blast radius
/// (the `impact` tool's default).
pub const RISK_IMPACT_DEPTH: u32 = 2;
/// Default depth searched for tests (the `tests` tool's default).
pub const RISK_TEST_DEPTH: u32 = 4;

/// One changed function or method.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RiskEntry {
    pub name: String,
    pub qualified_name: String,
    pub kind: &'static str,
    pub file: String,
    pub line: u32,
    /// Symbols within the impact depth that depend on it.
    pub dependents: usize,
    /// Test functions among its dependents within the test depth.
    pub tests: usize,
}

/// Result of [`risk_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RiskReport {
    /// What the working tree was compared with (`HEAD`, a branch, a commit).
    pub base: String,
    /// Changed indexed source files (test files excluded).
    pub changed_files: usize,
    /// Changed functions and methods measured.
    pub functions: usize,
    /// Of those, the ones no test reaches.
    pub untested: usize,
    pub impact_depth: u32,
    pub test_depth: u32,
    /// Untested first, most dependents first; then tested, fewest tests per
    /// dependent first.
    pub entries: Vec<RiskEntry>,
    /// Entries left out by `top` / `--untested`.
    pub entries_omitted: usize,
    pub note: String,
}

/// Measure the functions changed since `base` in `project_root`'s index.
pub fn risk_report(
    cg: &CodeGraph,
    project_root: &Path,
    base: &str,
    impact_depth: u32,
    test_depth: u32,
) -> Result<RiskReport, String> {
    let files = changed_files(project_root, base)?;
    let mut entries = Vec::new();
    let mut measured_files = 0;
    for file in &files {
        if is_test_source_file(file) {
            continue;
        }
        let nodes = cg.get_nodes_in_file(file).map_err(|e| e.to_string())?;
        let functions: Vec<&Node> = nodes.iter().filter(|node| is_measured(node)).collect();
        if functions.is_empty() {
            continue;
        }
        measured_files += 1;
        for node in functions {
            entries.push(measure(cg, node, impact_depth, test_depth)?);
        }
    }
    rank(&mut entries);
    let untested = entries.iter().filter(|entry| entry.tests == 0).count();
    Ok(RiskReport {
        base: base.to_string(),
        changed_files: measured_files,
        functions: entries.len(),
        untested,
        impact_depth,
        test_depth,
        entries,
        entries_omitted: 0,
        note: format!(
            "Tests are found through the call graph ({test_depth} hops): code reached only \
             through dynamic dispatch the index cannot see, or from deeper in a pipeline, \
             counts as untested."
        ),
    })
}

impl RiskReport {
    /// Keep the first `top` entries (only untested ones when `untested_only`).
    pub fn limit(&mut self, top: usize, untested_only: bool) {
        let before = self.entries.len();
        if untested_only {
            self.entries.retain(|entry| entry.tests == 0);
        }
        self.entries.truncate(top);
        self.entries_omitted = before - self.entries.len();
    }
}

fn is_measured(node: &Node) -> bool {
    matches!(node.kind, NodeKind::Function | NodeKind::Method)
        && !is_test_symbol(&node.file_path, &node.qualified_name)
}

fn measure(
    cg: &CodeGraph,
    node: &Node,
    impact_depth: u32,
    test_depth: u32,
) -> Result<RiskEntry, String> {
    let radius = cg
        .get_impact_radius(&node.id, Some(impact_depth))
        .map_err(|e| e.to_string())?;
    let reach = cg
        .get_impact_radius(&node.id, Some(test_depth))
        .map_err(|e| e.to_string())?;
    let tests = reach
        .nodes
        .values()
        .filter(|dependent| {
            matches!(dependent.kind, NodeKind::Function | NodeKind::Method)
                && is_test_symbol(&dependent.file_path, &dependent.qualified_name)
        })
        .count();
    Ok(RiskEntry {
        name: node.name.clone(),
        qualified_name: node.qualified_name.clone(),
        kind: node.kind.as_str(),
        file: node.file_path.clone(),
        line: node.start_line,
        dependents: radius.nodes.len().saturating_sub(1),
        tests,
    })
}

/// Untested first by dependents; tested by tests per dependent, ascending.
fn rank(entries: &mut [RiskEntry]) {
    entries.sort_by(|a, b| {
        let untested = |entry: &RiskEntry| entry.tests == 0;
        untested(b)
            .cmp(&untested(a))
            .then_with(|| {
                if untested(a) {
                    b.dependents.cmp(&a.dependents)
                } else {
                    // a.tests / a.dependents < b.tests / b.dependents
                    (a.tests * b.dependents.max(1)).cmp(&(b.tests * a.dependents.max(1)))
                }
            })
            .then_with(|| b.dependents.cmp(&a.dependents))
            .then_with(|| (&a.file, a.line).cmp(&(&b.file, b.line)))
    });
}

/// Files changed since `base` (and untracked ones), relative to the root
/// (`git diff --relative <base>` plus `git ls-files --others`).
pub fn changed_files(project_root: &Path, base: &str) -> Result<Vec<String>, String> {
    let diff = git(
        project_root,
        &["diff", "--name-only", "--relative", base, "--"],
    )?;
    let untracked = git(
        project_root,
        &["ls-files", "--others", "--exclude-standard"],
    )?;
    let files: BTreeSet<String> = diff
        .lines()
        .chain(untracked.lines())
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    Ok(files.into_iter().collect())
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, dependents: usize, tests: usize) -> RiskEntry {
        RiskEntry {
            name: name.into(),
            qualified_name: name.into(),
            kind: "function",
            file: "src/lib.rs".into(),
            line: 1,
            dependents,
            tests,
        }
    }

    #[test]
    fn untested_code_ranks_first_by_blast_radius() {
        let mut entries = vec![
            entry("well_tested", 10, 20),
            entry("untested_leaf", 1, 0),
            entry("thinly_tested", 10, 1),
            entry("untested_hub", 30, 0),
        ];
        rank(&mut entries);
        let order: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            order,
            [
                "untested_hub",
                "untested_leaf",
                "thinly_tested",
                "well_tested"
            ]
        );
    }

    #[test]
    fn limit_counts_what_it_leaves_out() {
        let mut report = RiskReport {
            base: "HEAD".into(),
            changed_files: 1,
            functions: 3,
            untested: 1,
            impact_depth: 2,
            test_depth: 4,
            entries: vec![entry("a", 3, 0), entry("b", 2, 1), entry("c", 1, 1)],
            entries_omitted: 0,
            note: String::new(),
        };
        report.limit(10, true);
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries_omitted, 2);
    }
}

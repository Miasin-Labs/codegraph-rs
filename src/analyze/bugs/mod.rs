//! `codegraph analyze bugs`: logic-bug detectors over the index and the
//! sources, beyond what a call graph shows.
//!
//! Two families, both reported as [`Finding`]s:
//!
//! - **Deviance** ([`deviance`]): beliefs mined from the code itself, after
//!   Engler et al., "Bugs as Deviant Behavior" — what most call sites of a
//!   function do with its result, which calls usually accompany it, what
//!   most arms of a `match` do. A site that departs from a strong belief is a
//!   likely bug, with the agreeing sites as evidence. No spec needed.
//! - **Lint** ([`lint`]): classic bug shapes read from the syntax, per
//!   language rule tables — a loop whose condition nothing in its body can
//!   change, branches with identical bodies, stores overwritten before any
//!   read, comparisons of a value with itself, constant conditions.
//!
//! Both build on what is precise: resolved call edges from the index (a
//! caller, a callee, and the line and column of each call site — every
//! language the indexer resolves) and tree-sitter syntax re-parsed per file.
//! The analysis crate's IR carries no lines, `match` or `break`, so it is not
//! used here.

mod deviance;
mod lint;
mod project;
mod review;

use std::collections::BTreeMap;
use std::path::Path;

pub use project::{CallSite, FnSpan, Project};
pub use review::{ReviewPacket, review_packets};
use serde::Serialize;

use crate::codegraph::CodeGraph;

/// Which detector family found it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Detector {
    Deviance,
    Lint,
}

/// A place that supports (or contradicts) a finding: another call site that
/// uses the result, the sibling arms of a match, the store that is lost.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    pub file: String,
    pub line: u32,
    pub note: String,
}

/// One suspected bug.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    pub detector: Detector,
    /// Stable id of the rule (`result-discarded`, `loop-no-progress`, …).
    pub rule: &'static str,
    pub file: String,
    pub line: u32,
    pub col: u32,
    /// The enclosing function or method (qualified name), when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    pub message: String,
    /// How strongly the code's own behaviour backs the finding, 0..=1
    /// (deviance: the belief's agreement; lint: the rule's precision class).
    pub confidence: f64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
}

/// What to run and what to report.
#[derive(Debug, Clone, Default)]
pub struct BugsOptions {
    /// Run only these detector families (all when empty).
    pub detectors: Vec<Detector>,
    /// Report only findings in these files (project-relative), e.g. the
    /// files changed since a git base. Beliefs are still learned from the
    /// whole project.
    pub only_files: Option<Vec<String>>,
    /// Include findings inside test code.
    pub include_tests: bool,
}

/// Result of [`bugs_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BugsReport {
    pub files_scanned: usize,
    pub functions: usize,
    pub call_sites: usize,
    /// Most confident first.
    pub findings: Vec<Finding>,
    /// Findings left out by `top`.
    pub findings_omitted: usize,
    /// Findings per rule (before `top`).
    pub by_rule: BTreeMap<&'static str, usize>,
    /// Files not analysed, by reason (unsupported language, unreadable…).
    pub skipped: BTreeMap<String, usize>,
    pub note: String,
}

impl BugsReport {
    /// Keep the `top` most confident findings.
    pub fn limit(&mut self, top: usize) {
        let before = self.findings.len();
        self.findings.truncate(top);
        self.findings_omitted = before - self.findings.len();
    }
}

/// Run the detectors over the project indexed at `project_root`.
pub fn bugs_report(
    cg: &CodeGraph,
    project_root: &Path,
    options: &BugsOptions,
) -> Result<BugsReport, String> {
    let mut project = Project::load(cg, project_root)?;
    let findings = detect(&mut project, options);
    let mut by_rule = BTreeMap::new();
    for finding in &findings {
        *by_rule.entry(finding.rule).or_default() += 1;
    }

    Ok(BugsReport {
        files_scanned: project.files_parsed(),
        functions: project.function_count(),
        call_sites: project.call_sites().len(),
        findings,
        findings_omitted: 0,
        by_rule,
        skipped: project.skipped().clone(),
        note: "Deviance findings are departures from what the rest of the code does, with the \
               agreeing sites as evidence; lint findings are syntactic bug shapes. Both are \
               leads to confirm by reading the code (`codegraph analyze review`), not proofs."
            .to_string(),
    })
}

/// Which findings to turn into review packets.
#[derive(Debug, Clone, Default)]
pub struct ReviewSelection {
    /// Only this rule.
    pub rule: Option<String>,
    /// Only findings at this file (project-relative), and line when given.
    pub at: Option<(String, Option<u32>)>,
    /// At most this many packets, most confident first.
    pub top: usize,
}

/// Run the detectors and build a [`ReviewPacket`] for each selected finding.
pub fn bugs_review(
    cg: &CodeGraph,
    project_root: &Path,
    options: &BugsOptions,
    selection: &ReviewSelection,
) -> Result<Vec<ReviewPacket>, String> {
    let mut project = Project::load(cg, project_root)?;
    let mut findings = detect(&mut project, options);
    findings.retain(|finding| {
        selection
            .rule
            .as_deref()
            .is_none_or(|rule| finding.rule == rule)
            && selection.at.as_ref().is_none_or(|(file, line)| {
                &finding.file == file && line.is_none_or(|line| finding.line == line)
            })
    });
    findings.truncate(selection.top.max(1));
    Ok(review_packets(&mut project, &findings))
}

/// The selected detectors' findings, filtered by `options`, most confident
/// first.
fn detect(project: &mut Project, options: &BugsOptions) -> Vec<Finding> {
    let wants = |detector| options.detectors.is_empty() || options.detectors.contains(&detector);
    let mut findings = Vec::new();
    if wants(Detector::Deviance) {
        findings.extend(deviance::detect(project));
    }
    if wants(Detector::Lint) {
        findings.extend(lint::detect(project));
    }
    findings.retain(|finding| {
        options
            .only_files
            .as_ref()
            .is_none_or(|files| files.iter().any(|file| file == &finding.file))
            && (options.include_tests || !project.is_test_location(finding))
    });
    findings.sort_by(|a, b| {
        b.confidence
            .total_cmp(&a.confidence)
            .then_with(|| (&a.file, a.line, a.col).cmp(&(&b.file, b.line, b.col)))
    });
    findings
}

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
//! - **Rule** (`crate::analyze::rules`): YAML rules — weggli patterns and
//!   tree-sitter queries with semantic predicates — run by
//!   `codegraph analyze rules`; their findings share this type so ranking
//!   and review packets work the same.
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

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

pub use project::{CallSite, FnSpan, Project};
pub use review::{ReviewPacket, review_packets};
use serde::Serialize;

use crate::codegraph::CodeGraph;
use crate::resolution::line_index::LineStarts;

/// Which detector family found it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Detector {
    Deviance,
    Lint,
    /// A YAML rule (`codegraph analyze rules`).
    Rule,
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
    /// Stable id of the rule (`result-discarded`, `loop-no-progress`, a
    /// YAML rule's `id`…).
    pub rule: Cow<'static, str>,
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
    /// Time the taint rules' pass may take (a default when `None`); spent,
    /// it reports what it found and says the rest was not followed.
    pub taint_budget: Option<std::time::Duration>,
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
    pub by_rule: BTreeMap<String, usize>,
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
    Ok(report(
        &project,
        project.files_parsed(),
        findings,
        "Deviance findings are departures from what the rest of the code does, with the \
         agreeing sites as evidence; lint findings are syntactic bug shapes. Both are leads to \
         confirm by reading the code (`codegraph analyze review`), not proofs.",
    ))
}

/// A report of `findings` (already filtered and ranked) over `project`.
pub(crate) fn report(
    project: &Project,
    files_scanned: usize,
    findings: Vec<Finding>,
    note: &str,
) -> BugsReport {
    let mut by_rule = BTreeMap::new();
    for finding in &findings {
        *by_rule.entry(finding.rule.to_string()).or_default() += 1;
    }
    BugsReport {
        files_scanned,
        functions: project.function_count(),
        call_sites: project.call_sites().len(),
        findings,
        findings_omitted: 0,
        by_rule,
        skipped: project.skipped().clone(),
        note: note.to_string(),
    }
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

/// Run the detectors (and `rules`, when given) and build a
/// [`ReviewPacket`] for each selected finding.
pub fn bugs_review(
    cg: &CodeGraph,
    project_root: &Path,
    options: &BugsOptions,
    selection: &ReviewSelection,
    rules: Option<&crate::analyze::rules::RuleSet>,
) -> Result<Vec<ReviewPacket>, String> {
    let mut project = Project::load(cg, project_root)?;
    let mut findings = detect(&mut project, options);
    if let Some(rules) = rules {
        findings.extend(crate::analyze::rules::detect(
            cg,
            &mut project,
            rules,
            options,
        )?);
        rank(&mut findings);
    }
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
    Ok(review_packets(&mut project, &findings, &|finding| {
        rules
            .map(|rules| crate::analyze::rules::review_questions(rules, finding))
            .unwrap_or_default()
    }))
}

/// The selected detectors' findings, filtered by `options`, most confident
/// first.
pub(crate) fn detect(project: &mut Project, options: &BugsOptions) -> Vec<Finding> {
    // `Rule` findings come from the rules engine: asking for only those
    // runs neither family here.
    let wants = |detector| options.detectors.is_empty() || options.detectors.contains(&detector);
    let mut findings = Vec::new();
    if wants(Detector::Deviance) {
        findings.extend(deviance::detect(project));
    }
    if wants(Detector::Lint) {
        findings.extend(lint::detect(project));
    }
    retain_selected(project, &mut findings, options);
    rank(&mut findings);
    findings
}

/// Keep the findings `options` selects: in `only_files`, outside test code
/// unless `include_tests`.
pub(crate) fn retain_selected(
    project: &mut Project,
    findings: &mut Vec<Finding>,
    options: &BugsOptions,
) {
    // Test items the syntax marks, and suppression comments, are read from
    // the parsed file.
    let files: HashSet<String> = findings.iter().map(|f| f.file.clone()).collect();
    let mut line_starts: HashMap<String, LineStarts> = HashMap::new();
    for file in files {
        if let Some(parsed) = project.parsed(&file) {
            line_starts.insert(file, LineStarts::new(&parsed.source));
        }
    }
    let project = &*project;
    findings.retain(|finding| {
        let suppressed = project
            .parsed_cached(&finding.file)
            .zip(line_starts.get(&finding.file))
            .is_some_and(|(parsed, starts)| {
                suppressed_at(&parsed.source, starts, finding.line, &finding.rule)
            });
        if suppressed {
            return false;
        }
        options
            .only_files
            .as_ref()
            .is_none_or(|files| files.iter().any(|file| file == &finding.file))
            && (options.include_tests || !project.is_test_location(finding))
    });
}

/// Whether `line` (1-based) or the line above it carries a suppression
/// comment for `rule`: `codegraph: ignore <rule-id>[, <rule-id>…]`, followed
/// by the reason (`// codegraph: ignore rust-http-client-follows-redirects —
/// the providers' downloads redirect`). The comment syntax is the
/// language's own; only the marker is read.
fn suppressed_at(source: &str, starts: &LineStarts, line: u32, rule: &str) -> bool {
    const MARKER: &str = "codegraph: ignore";
    [line.saturating_sub(1), line]
        .into_iter()
        .filter(|&at| at > 0)
        .any(|at| {
            let start = starts.line_start(at).min(source.len());
            let end = starts
                .line_start(at + 1)
                .saturating_sub(1)
                .min(source.len());
            let Some(text) = source.get(start..end.max(start)) else {
                return false;
            };
            text.match_indices(MARKER).any(|(i, _)| {
                text[i + MARKER.len()..]
                    .split(|c: char| c == ',' || c.is_whitespace())
                    .filter(|word| !word.is_empty())
                    .take_while(|word| {
                        word.chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                    })
                    .any(|word| word == rule)
            })
        })
}

/// Most confident first, then by place.
pub(crate) fn rank(findings: &mut [Finding]) {
    findings.sort_by(|a, b| {
        b.confidence
            .total_cmp(&a.confidence)
            .then_with(|| (&a.file, a.line, a.col).cmp(&(&b.file, b.line, b.col)))
            .then_with(|| a.rule.cmp(&b.rule))
    });
}

#[cfg(test)]
mod suppression_tests {
    use super::*;

    #[test]
    fn a_suppression_comment_names_the_rule_on_its_line_or_the_line_above() {
        let source = "fn a() {\n\
                      // codegraph: ignore rule-a, rule-b — both are fine here\n\
                      call();\n\
                      other(); // codegraph: ignore rule-c\n\
                      last();\n}\n";
        let starts = LineStarts::new(source);
        let at = |line, rule| suppressed_at(source, &starts, line, rule);
        assert!(at(3, "rule-a") && at(3, "rule-b"));
        assert!(!at(3, "rule-c"), "another rule stays reported");
        assert!(at(4, "rule-c"), "a trailing comment covers its own line");
        assert!(!at(5, "rule-a"), "only the next line is covered");
        assert!(!at(3, "rule"), "ids match whole");
        assert!(!at(99, "rule-a"), "past the end");
    }
}

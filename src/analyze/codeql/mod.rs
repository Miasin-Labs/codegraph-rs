//! CodeQL as a detector family: codegraph *uses* GitHub's CodeQL instead of
//! reimplementing it, and adds what the graph knows.
//!
//! `codegraph analyze codeql` (and `analyze bugs|review --detector codeql`)
//! finds the CodeQL CLI ([`locate`]; never downloaded or installed), picks
//! the CodeQL languages of the indexed files ([`languages`]; `--build-mode=
//! none` wherever the extractor supports it, else `autobuild`), and brings
//! one database and one SARIF file per language up to date in a detached,
//! bounded run cached by a source fingerprint under `.codegraph/codeql/`
//! ([`run`]; a later call picks up a run still going). The SARIF is read by
//! [`crate::analyze::sarif::read`] and mapped by [`findings`]: rule
//! `codeql::<query id>`, confidence from the query's `precision` and
//! `security-severity`, the enclosing indexed function, a taint path's hops
//! as evidence, and reachability from server entry points
//! (`bugs::reach`). Suppressions (`codegraph: ignore codeql::java/xss`) and
//! test-code filtering apply as for every detector, and a result at the
//! same place and CWE as a codegraph rule's finding merges with it
//! ([`merge_corroborated`]), each engine kept as evidence.
//!
//! **Licensing.** The CodeQL CLI is not open source. Its terms
//! (<https://securitylab.github.com/tools/codeql/license>) allow running it
//! on open-source code, for academic research, and to test or demonstrate
//! the software — not on other private code. The adapter is opt-in (never
//! in the default detector set) and says so in its help.
//!
//! Heavy work: the CLI only, never MCP or the prompt hook.

pub mod findings;
pub mod languages;
pub mod locate;
pub mod run;
#[cfg(test)]
mod sample_tests;

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Serialize;

use self::languages::CodeqlLanguage;
pub use self::run::{LangOutcome, StepState};
use crate::analyze::bugs::reach::EntryKind;
use crate::analyze::bugs::{Finding, Project};
use crate::analyze::sarif::read::{self, RuleMeta};
use crate::analyze::sarif::write::RuleDoc;

/// The license note every surface repeats.
pub const LICENSE_NOTE: &str = "CodeQL is GitHub's, under the GitHub CodeQL Terms and \
    Conditions: use it on open-source code, for academic research, or to test or demonstrate \
    the software — not on other private code \
    (https://securitylab.github.com/tools/codeql/license).";

/// How long a call waits for CodeQL when the caller sets no limit. The run
/// keeps going past it; the next call picks it up.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(300);

/// What to run.
#[derive(Debug, Clone)]
pub struct CodeqlOptions {
    /// CodeQL languages (`java`, `javascript`, `python`…; CodeQL's aliases
    /// too). Empty: every language of the indexed files.
    pub languages: Vec<String>,
    /// Suites, packs or query files for `database analyze`. Empty: each
    /// language's `security-and-quality` suite.
    pub suites: Vec<String>,
    /// How long to wait for a run before reporting it `running`.
    pub wait: Duration,
}

impl Default for CodeqlOptions {
    fn default() -> Self {
        Self {
            languages: Vec::new(),
            suites: Vec::new(),
            wait: DEFAULT_WAIT,
        }
    }
}

/// Where the CodeQL run stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CodeqlState {
    /// Finished (now or earlier); per-language outcomes say what came out.
    Complete,
    /// Still running in the background; call again for the findings.
    Running,
    /// No CodeQL CLI, or nothing it can analyse.
    Unavailable,
}

/// The CodeQL run behind a report's `codeql::` findings.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeqlStatus {
    pub state: CodeqlState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub languages: Vec<LangOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_ms: Option<u64>,
    /// Sources changed during the run; call again for current results.
    pub stale: bool,
    /// SARIF results read (before mapping and filtering).
    pub results: usize,
    /// Results outside the indexed files (not findings).
    pub outside_index: usize,
    /// Entry points reachability was measured from, and their kind.
    pub entry_points: usize,
    pub entry_kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    pub license: &'static str,
}

impl CodeqlStatus {
    fn unavailable(why: String) -> Self {
        Self {
            state: CodeqlState::Unavailable,
            tool: None,
            version: None,
            languages: Vec::new(),
            started_ms: None,
            finished_ms: None,
            stale: false,
            results: 0,
            outside_index: 0,
            entry_points: 0,
            entry_kind: "none",
            failure: Some(why),
            license: LICENSE_NOTE,
        }
    }

    /// One line for a person.
    pub fn summary(&self) -> String {
        match self.state {
            CodeqlState::Unavailable => format!(
                "CodeQL unavailable: {}",
                self.failure.as_deref().unwrap_or("unknown reason")
            ),
            CodeqlState::Running => "CodeQL is still running in the background; run again to \
                                     pick up its findings"
                .to_string(),
            CodeqlState::Complete => {
                let langs: Vec<String> = self
                    .languages
                    .iter()
                    .map(|lang| {
                        let how = match lang.state {
                            StepState::Complete if lang.cached => "cached".to_string(),
                            StepState::Complete => {
                                let secs =
                                    lang.create_secs.unwrap_or(0) + lang.analyze_secs.unwrap_or(0);
                                format!("{secs}s")
                            }
                            StepState::Running => "running".to_string(),
                            StepState::Failed => "FAILED".to_string(),
                        };
                        format!(
                            "{} (build mode {}, {how}, db {:.0} MiB)",
                            lang.language,
                            lang.build_mode.as_str(),
                            lang.database_bytes as f64 / (1024.0 * 1024.0)
                        )
                    })
                    .collect();
                let mut line = format!(
                    "CodeQL {}: {} — {} result{}{}; reachability from {} entry point{} ({})",
                    self.version.as_deref().unwrap_or("?"),
                    langs.join(", "),
                    self.results,
                    if self.results == 1 { "" } else { "s" },
                    if self.outside_index > 0 {
                        format!(" ({} outside the indexed files)", self.outside_index)
                    } else {
                        String::new()
                    },
                    self.entry_points,
                    if self.entry_points == 1 { "" } else { "s" },
                    self.entry_kind
                );
                if self.stale {
                    line.push_str("; sources changed during the run — run again");
                }
                line
            }
        }
    }
}

/// A CodeQL pass over a project.
#[derive(Debug)]
pub struct CodeqlRun {
    pub findings: Vec<Finding>,
    pub status: CodeqlStatus,
    /// Rule descriptors by `codeql::<id>` (for SARIF out and merging).
    pub rules: BTreeMap<String, RuleMeta>,
}

/// What a CodeQL rule's descriptor becomes in codegraph's SARIF and in
/// [`merge_corroborated`]'s classes.
pub fn rule_doc(id: &str, meta: &RuleMeta) -> RuleDoc {
    RuleDoc {
        id: id.to_string(),
        short: meta
            .short_description
            .clone()
            .or_else(|| meta.name.clone())
            .unwrap_or_else(|| id.to_string()),
        full: meta.full_description.clone(),
        help_uri: meta.help_uri.clone(),
        tags: meta.tags.clone(),
        precision: meta.precision.clone(),
        security_severity: meta.security_severity,
    }
}

/// Run (or pick up, or read from the cache) CodeQL over `project`.
pub fn detect(project: &Project, options: &CodeqlOptions) -> CodeqlRun {
    let unavailable = |why: String| CodeqlRun {
        findings: Vec::new(),
        status: CodeqlStatus::unavailable(why),
        rules: BTreeMap::new(),
    };
    let tool = match locate::find() {
        Ok(tool) => tool,
        Err(why) => return unavailable(why),
    };
    let root = project.root().to_path_buf();

    // Languages: the ones named, else the indexed files'.
    let chosen: Vec<(&'static CodeqlLanguage, Vec<String>)> = if options.languages.is_empty() {
        languages::detect(project.files())
    } else {
        let mut chosen = Vec::new();
        for name in &options.languages {
            let Some(lang) = languages::by_id(name) else {
                let known: Vec<&str> = languages::LANGUAGES.iter().map(|l| l.id).collect();
                return unavailable(format!(
                    "unknown CodeQL language `{name}` (known: {})",
                    known.join(", ")
                ));
            };
            if !chosen
                .iter()
                .any(|(l, _): &(&CodeqlLanguage, Vec<String>)| l.id == lang.id)
            {
                chosen.push((lang, languages::files_of(lang, project.files())));
            }
        }
        chosen
    };
    if chosen.is_empty() {
        return unavailable(
            "no indexed files in a language CodeQL analyses (java, javascript/typescript, \
             python, ruby, go, c/c++, c#, rust, swift)"
                .to_string(),
        );
    }
    let suites: Vec<&str> = if options.suites.is_empty() {
        vec![languages::DEFAULT_SUITE]
    } else {
        options.suites.iter().map(String::as_str).collect()
    };
    let mut plans = Vec::new();
    for (lang, files) in chosen {
        let Some(mode) = languages::build_mode(&tool.dist, lang) else {
            continue;
        };
        let specs = suites
            .iter()
            .map(|suite| languages::suite_spec(lang, suite))
            .collect();
        plans.push(run::LangPlan::new(&root, &tool, lang, mode, &files, specs));
    }
    if plans.is_empty() {
        return unavailable(format!(
            "CodeQL {} has no usable build mode for these languages",
            tool.version
        ));
    }

    let outcome = match run::ensure(&root, &tool, &plans, options.wait) {
        Ok(outcome) => outcome,
        Err(why) => return unavailable(why),
    };
    let mut status = CodeqlStatus {
        state: if outcome.running {
            CodeqlState::Running
        } else {
            CodeqlState::Complete
        },
        tool: Some(tool.exe.to_string_lossy().into_owned()),
        version: Some(tool.version.clone()),
        languages: outcome.languages,
        started_ms: outcome.started_ms,
        finished_ms: outcome.finished_ms,
        stale: outcome.stale,
        results: 0,
        outside_index: 0,
        entry_points: 0,
        entry_kind: "none",
        failure: None,
        license: LICENSE_NOTE,
    };
    if outcome.running {
        return CodeqlRun {
            findings: Vec::new(),
            status,
            rules: BTreeMap::new(),
        };
    }
    let mut runs = Vec::new();
    for lang in &mut status.languages {
        let Some(path) = lang.sarif.as_ref() else {
            continue;
        };
        match std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|text| read::parse(&text))
        {
            Ok(parsed) => runs.extend(parsed),
            Err(why) => {
                lang.state = StepState::Failed;
                lang.failure = Some(format!("unreadable SARIF {}: {why}", path.display()));
            }
        }
    }
    status.results = runs.iter().map(|run| run.results.len()).sum();
    let mapped = findings::map_runs(project, &runs);
    status.outside_index = mapped.outside_index + mapped.unlocated;
    let reach = project.reach();
    status.entry_points = reach.entries.len();
    status.entry_kind = match reach.strongest_kind() {
        Some(EntryKind::Route) => "routes",
        Some(EntryKind::Extractor) => "request extractors",
        Some(EntryKind::Listener) => "listeners",
        Some(EntryKind::Message) => "message handlers",
        Some(EntryKind::PublicApi) => "public API functions",
        None => "none found",
    };
    let failed: Vec<String> = status
        .languages
        .iter()
        .filter(|lang| lang.state == StepState::Failed)
        .map(|lang| {
            format!(
                "{}: {}",
                lang.language,
                lang.failure.as_deref().unwrap_or("failed")
            )
        })
        .collect();
    if !failed.is_empty() {
        status.failure = Some(failed.join("\n"));
    }
    CodeqlRun {
        findings: mapped.findings,
        status,
        rules: mapped.rules,
    }
}

/// `codegraph analyze codeql`: CodeQL's findings over the project indexed
/// at `project_root` (`options.detectors` is ignored: CodeQL runs), and
/// with `rules`, codegraph's rule findings too, merged where both engines
/// report one place and CWE ([`merge_corroborated`]).
pub fn codeql_report(
    cg: &crate::codegraph::CodeGraph,
    project_root: &std::path::Path,
    options: &crate::analyze::bugs::BugsOptions,
    rules: Option<&crate::analyze::rules::RuleSet>,
) -> Result<crate::analyze::bugs::BugsReport, String> {
    use crate::analyze::bugs::{self, Detector};
    let mut project = Project::load(cg, project_root)?;
    let mut options = options.clone();
    options.detectors = vec![Detector::CodeQL];
    let (mut found, extras) = bugs::detect_all(&mut project, &options);
    let mut docs = extras.rule_docs;
    if let Some(rules) = rules {
        options.detectors = vec![Detector::Rule];
        found.extend(crate::analyze::rules::detect(
            cg,
            &mut project,
            rules,
            &options,
        )?);
        docs.extend(crate::analyze::rules::rule_docs(rules));
        merge_corroborated(&mut found, &bugs::classes_of(&docs));
    }
    bugs::rank(&mut found);
    let status = extras
        .codeql
        .unwrap_or_else(|| CodeqlStatus::unavailable("CodeQL did not run".to_string()));
    let mut note = format!(
        "CodeQL findings (`codeql::…`) are CodeQL query results in the graph's terms: the \
         enclosing function, a taint path's hops as evidence, reachability from entry points. \
         {}.",
        status.summary()
    );
    if rules.is_some() {
        note.push_str(
            " Rule findings ran too; a place both engines report for one CWE is one finding \
             with the other engine as evidence.",
        );
    }
    note.push(' ');
    note.push_str(LICENSE_NOTE);
    let mut report = bugs::report(&project, project.files_parsed(), found, &note);
    report.rule_docs = docs;
    report.codeql = Some(status);
    Ok(report)
}

/// The review checklist a CodeQL finding adds.
pub fn review_questions(finding: &Finding) -> Vec<String> {
    use crate::analyze::bugs::Detector;
    if finding.detector != Detector::CodeQL {
        return Vec::new();
    }
    let has_path = finding
        .evidence
        .iter()
        .any(|e| e.note.starts_with("source ") || e.note.starts_with("sink "));
    let mut questions = Vec::new();
    if has_path {
        questions.push(format!(
            "CodeQL's `{}` traced a path (the source/step/sink evidence). Does data an \
             attacker controls really arrive at the source, and does nothing on the way — a \
             check, a type, a constant branch CodeQL does not model — make it safe?",
            finding.rule
        ));
    } else {
        questions.push(format!(
            "CodeQL's `{}` flags this. Is it wrong here, or deliberate (then `codegraph: \
             ignore {}` with the reason)?",
            finding.rule, finding.rule
        ));
    }
    questions
}

/// Merge findings two engines report at the same place and class: the same
/// file and enclosing function (or within 3 lines when there is none), one
/// from CodeQL and one from another detector, sharing a CWE of their rules
/// (`classes`: rule id → CWEs). The more confident one stays, with
/// confidence `1 − (1 − a)(1 − b)` (≤ 0.99) and the other as evidence;
/// every finding is merged at most once.
pub fn merge_corroborated(findings: &mut Vec<Finding>, classes: &BTreeMap<String, Vec<String>>) {
    use crate::analyze::bugs::{Detector, Evidence};
    let none: Vec<String> = Vec::new();
    let class_of: Vec<&Vec<String>> = findings
        .iter()
        .map(|f| classes.get(f.rule.as_ref()).unwrap_or(&none))
        .collect();
    let same_place = |a: &Finding, b: &Finding| match (&a.function, &b.function) {
        (Some(x), Some(y)) => x == y,
        (None, None) => a.line.abs_diff(b.line) <= 3,
        _ => false,
    };
    // Candidates by file, so a finding is compared with its file's only.
    let mut by_file: std::collections::HashMap<&str, Vec<usize>> = std::collections::HashMap::new();
    for (index, finding) in findings.iter().enumerate() {
        if !class_of[index].is_empty() {
            by_file
                .entry(finding.file.as_str())
                .or_default()
                .push(index);
        }
    }
    let mut gone = vec![false; findings.len()];
    let mut merges: Vec<(usize, usize)> = Vec::new();
    for indices in by_file.values() {
        for &i in indices {
            if gone[i] || findings[i].detector != Detector::CodeQL {
                continue;
            }
            let partner = indices.iter().copied().find(|&j| {
                !gone[j]
                    && findings[j].detector != Detector::CodeQL
                    && same_place(&findings[i], &findings[j])
                    && class_of[j].iter().any(|cwe| class_of[i].contains(cwe))
            });
            if let Some(j) = partner {
                gone[i] = true;
                gone[j] = true;
                merges.push((i, j));
            }
        }
    }
    if merges.is_empty() {
        return;
    }
    let mut merged = Vec::new();
    for (i, j) in merges {
        let (a, b) = (&findings[i], &findings[j]);
        let (keep, other) = if b.confidence >= a.confidence {
            (b, a)
        } else {
            (a, b)
        };
        let mut kept = keep.clone();
        let combined = 1.0 - (1.0 - keep.confidence) * (1.0 - other.confidence);
        kept.confidence = (combined.min(0.99) * 1000.0).round() / 1000.0;
        kept.evidence.insert(
            0,
            Evidence {
                file: other.file.clone(),
                line: other.line,
                note: format!(
                    "also reported by {} as `{}`: {}",
                    serde_json::to_value(other.detector)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_default(),
                    other.rule,
                    other.message
                ),
            },
        );
        merged.push(kept);
    }
    let mut index = 0;
    findings.retain(|_| {
        let keep = !gone[index];
        index += 1;
        keep
    });
    findings.extend(merged);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::bugs::{Detector, Evidence};

    fn finding(detector: Detector, rule: &str, line: u32, confidence: f64) -> Finding {
        Finding {
            detector,
            rule: rule.to_string().into(),
            file: "src/A.java".into(),
            line,
            col: 0,
            function: Some("A.doPost".into()),
            message: format!("{rule} here"),
            confidence,
            evidence: vec![Evidence {
                file: "src/A.java".into(),
                line,
                note: "sink".into(),
            }],
        }
    }

    #[test]
    fn same_place_and_class_merge_keeping_both_engines() {
        let classes: BTreeMap<String, Vec<String>> = [
            ("codeql::java/sql-injection", vec!["CWE-89"]),
            ("java-sqli-taint", vec!["CWE-89"]),
            ("codeql::java/xss", vec!["CWE-79"]),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.into_iter().map(String::from).collect()))
        .collect();
        let mut findings = vec![
            finding(Detector::CodeQL, "codeql::java/sql-injection", 70, 0.8),
            finding(Detector::Rule, "java-sqli-taint", 71, 0.9),
            finding(Detector::CodeQL, "codeql::java/xss", 80, 0.76),
            finding(Detector::Lint, "java-catch-generic-exception", 90, 0.5),
        ];
        merge_corroborated(&mut findings, &classes);
        assert_eq!(findings.len(), 3, "{findings:?}");
        let merged = findings
            .iter()
            .find(|f| f.rule == "java-sqli-taint")
            .unwrap();
        assert_eq!(merged.confidence, 0.98);
        assert!(
            merged.evidence[0]
                .note
                .starts_with("also reported by codeql as `codeql::java/sql-injection`"),
            "{:?}",
            merged.evidence
        );
        assert!(findings.iter().any(|f| f.rule == "codeql::java/xss"));
        assert!(
            !findings
                .iter()
                .any(|f| f.rule == "codeql::java/sql-injection"),
            "the merged CodeQL result is evidence now"
        );
    }

    #[test]
    fn a_missing_cli_is_unavailable_with_the_license() {
        let status = CodeqlStatus::unavailable(locate::INSTALL_HINT.to_string());
        assert_eq!(status.state, CodeqlState::Unavailable);
        assert!(status.summary().contains("CODEGRAPH_CODEQL"));
        assert!(status.summary().contains("open-source code"));
    }
}

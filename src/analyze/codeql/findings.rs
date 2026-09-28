//! CodeQL's SARIF results as codegraph [`Finding`]s, with the graph's
//! context: the enclosing indexed function, the taint path's hops and
//! related places as evidence, a confidence from the query's own metadata,
//! and reachability from the project's entry points.

use std::collections::{BTreeMap, HashSet};

use crate::analyze::bugs::reach::{EntryKind, Reached};
use crate::analyze::bugs::{Detector, Evidence, Finding, Project};
use crate::analyze::sarif::read::{RuleMeta, SarifLocation, SarifRun};

/// What the results became.
#[derive(Debug, Default)]
pub struct Mapped {
    pub findings: Vec<Finding>,
    /// Results in files the index does not hold (generated, ignored, or
    /// outside the project) — not findings.
    pub outside_index: usize,
    /// Results with no location at all.
    pub unlocated: usize,
    /// Every rule the runs describe, by `codeql::<id>`.
    pub rules: BTreeMap<String, RuleMeta>,
}

/// The finding rule for a CodeQL rule id.
pub fn rule_name(id: &str) -> String {
    format!("codeql::{id}")
}

/// The query's own estimate of how often it is right: its `precision`,
/// times how serious it says a hit is (`security-severity`, else
/// `problem.severity`, else the SARIF level).
pub fn base_confidence(meta: Option<&RuleMeta>, level: Option<&str>) -> f64 {
    let precision = match meta.and_then(|m| m.precision.as_deref()) {
        Some("very-high") => 0.9,
        Some("high") => 0.8,
        Some("medium") => 0.6,
        Some("low") => 0.4,
        _ => 0.5,
    };
    let severity = match meta.and_then(|m| m.security_severity) {
        Some(s) if s >= 9.0 => 1.0,
        Some(s) if s >= 7.0 => 0.95,
        Some(s) if s >= 4.0 => 0.9,
        Some(_) => 0.85,
        None => match meta.and_then(|m| m.problem_severity.as_deref()).or(level) {
            Some("error") => 0.95,
            Some("warning") => 0.85,
            Some("recommendation" | "note") => 0.7,
            _ => 0.8,
        },
    };
    round(precision * severity)
}

fn round(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

/// A security result a request handler (or listener, message handler)
/// reaches rises halfway toward 0.97, less per call away; anything else
/// keeps its query's confidence — CodeQL models its own sources, so an
/// unreached result is not discounted.
fn reached_confidence(base: f64, security: bool, reached: Option<&Reached<'_>>) -> f64 {
    match reached {
        Some(reached) if security && EntryKind::SERVER.contains(&reached.entry.kind) => {
            let decay = 0.85f64.powi(reached.depth as i32);
            round(base + (0.97 - base).max(0.0) * 0.5 * decay)
        }
        _ => base,
    }
}

/// The step's file relative to the project; `None` for a place outside it
/// (the JDK, a dependency jar, CodeQL's own working files).
fn place(project: &Project, loc: &SarifLocation) -> Option<String> {
    loc.relative_path(project.root())
        .filter(|path| !path.starts_with(".codegraph/"))
}

/// Map every result of `runs` onto `project`.
pub fn map_runs(project: &Project, runs: &[SarifRun]) -> Mapped {
    let indexed: HashSet<&str> = project.files().iter().map(String::as_str).collect();
    let reach = project.reach();
    let mut mapped = Mapped::default();
    for run in runs {
        for (id, meta) in &run.rules {
            mapped
                .rules
                .entry(rule_name(id))
                .or_insert_with(|| meta.clone());
        }
        for result in &run.results {
            let Some(location) = &result.location else {
                mapped.unlocated += 1;
                continue;
            };
            let Some(file) = location
                .relative_path(project.root())
                .filter(|file| indexed.contains(file.as_str()))
            else {
                mapped.outside_index += 1;
                continue;
            };
            let meta = run.rules.get(&result.rule_id);
            let line = location.line;
            let function = project.enclosing_function(&file, line);
            let reached = function.and_then(|span| reach.reached(&span.id));
            let security = meta.is_some_and(RuleMeta::is_security);

            let mut message = result.message.clone();
            let mut evidence = Vec::new();
            if let Some(flow) = result.flows.first() {
                let last = flow.len().saturating_sub(1);
                for (index, step) in flow.iter().enumerate() {
                    let role = step.role.as_deref().unwrap_or(if index == 0 {
                        "source"
                    } else if index == last {
                        "sink"
                    } else {
                        "step"
                    });
                    let what = step.message.as_deref().unwrap_or("");
                    let Some(step_file) = place(project, step) else {
                        continue;
                    };
                    evidence.push(Evidence {
                        file: step_file,
                        line: step.line,
                        note: format!("{role} {}/{}: {what}", index + 1, flow.len())
                            .trim_end_matches(": ")
                            .to_string(),
                    });
                }
                if result.flows.len() > 1 {
                    evidence.push(Evidence {
                        file: file.clone(),
                        line,
                        note: format!(
                            "{} more path{} to this sink",
                            result.flows.len() - 1,
                            if result.flows.len() == 2 { "" } else { "s" }
                        ),
                    });
                }
            }
            for related in &result.related {
                let Some(file) = place(project, related) else {
                    continue;
                };
                let duplicate = evidence
                    .iter()
                    .any(|e| e.file == file && e.line == related.line);
                if duplicate {
                    continue;
                }
                evidence.push(Evidence {
                    file,
                    line: related.line,
                    note: format!(
                        "related: {}",
                        related.message.as_deref().unwrap_or("(no message)")
                    ),
                });
            }
            if let Some(reached) = reached.as_ref().filter(|_| security) {
                if EntryKind::SERVER.contains(&reached.entry.kind) {
                    message.push_str(&format!(
                        " — reachable from {}{}",
                        reached.entry_text(),
                        reached.distance_text()
                    ));
                }
                evidence.push(Evidence {
                    file: reached.entry.file.clone(),
                    line: reached.entry.line,
                    note: format!("entry point: {}", reached.entry_text()),
                });
                for step in &reached.path {
                    evidence.push(Evidence {
                        file: step.file.clone(),
                        line: step.line,
                        note: format!("`{}` calls `{}`", step.caller, step.callee),
                    });
                }
            }
            let base = base_confidence(meta, result.level.as_deref());
            mapped.findings.push(Finding {
                detector: Detector::CodeQL,
                rule: rule_name(&result.rule_id).into(),
                file,
                line,
                col: location.column.saturating_sub(1),
                function: function.map(|span| span.qualified_name.clone()),
                message,
                confidence: reached_confidence(base, security, reached.as_ref()),
                evidence,
            });
        }
    }
    mapped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(precision: &str, security: Option<f64>, severity: &str) -> RuleMeta {
        RuleMeta {
            id: "x".into(),
            precision: Some(precision.into()),
            security_severity: security,
            problem_severity: Some(severity.into()),
            ..RuleMeta::default()
        }
    }

    #[test]
    fn confidence_is_precision_times_severity() {
        assert_eq!(
            base_confidence(Some(&meta("high", Some(9.8), "error")), None),
            0.8
        );
        assert_eq!(
            base_confidence(Some(&meta("high", Some(7.5), "error")), None),
            0.76
        );
        assert_eq!(
            base_confidence(Some(&meta("very-high", None, "recommendation")), None),
            0.63
        );
        assert_eq!(base_confidence(None, Some("warning")), 0.425);
        assert!(
            base_confidence(Some(&meta("low", Some(9.0), "error")), None)
                < base_confidence(Some(&meta("high", None, "warning")), None),
            "precision dominates"
        );
    }
}

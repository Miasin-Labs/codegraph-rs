//! Review packets: what a reader needs to confirm or dismiss a finding, and
//! nothing else — the enclosing function's numbered source (around the
//! finding when it is long), the evidence sites with a few lines of context,
//! who calls the function, and the questions the rule raises.
//!
//! Detectors point at a place; whether it is a bug depends on what the code
//! means (a conflict that must settle, a result that must be recorded), which
//! a reader — a person or a model — decides. The packet keeps that reading to
//! the lines that matter.

use serde::Serialize;

use super::{Finding, Project};

/// Longest function body a packet quotes whole; longer ones are windowed
/// around the finding.
const MAX_FUNCTION_LINES: u32 = 160;
/// Lines of context around each evidence site.
const EVIDENCE_CONTEXT: u32 = 3;
/// Callers of the enclosing function listed.
const MAX_CALLERS: usize = 8;

/// A few numbered source lines.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snippet {
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
    /// `N\ttext` lines, as a Read shows them.
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Where the enclosing function is called from.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallerRef {
    pub name: String,
    pub file: String,
    pub line: u32,
}

/// Everything to review one finding.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewPacket {
    pub finding: Finding,
    /// The enclosing function (or a window of it around the finding).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<Snippet>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Snippet>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub callers: Vec<CallerRef>,
    /// What to decide, for this rule.
    pub checklist: Vec<String>,
    /// Undefined behaviour Miri proved in the same function (the last
    /// `codegraph analyze miri` run): a static finding it confirms.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub confirmed_by: Vec<Finding>,
}

/// The Miri findings (`miri`) in the function holding `finding`, other
/// than `finding` itself.
pub fn confirmations(project: &Project, finding: &Finding, miri: &[Finding]) -> Vec<Finding> {
    let Some(span) = project.enclosing_function(&finding.file, finding.line) else {
        return Vec::new();
    };
    miri.iter()
        .filter(|proof| {
            proof.file == finding.file
                && span.contains(proof.line)
                && !(proof.rule == finding.rule && proof.line == finding.line)
        })
        .cloned()
        .collect()
}

/// A packet per finding, in the findings' order. `questions` adds a
/// finding's own questions (a YAML rule's `review`) to its checklist.
pub fn review_packets(
    project: &mut Project,
    findings: &[Finding],
    questions: &dyn Fn(&Finding) -> Vec<String>,
) -> Vec<ReviewPacket> {
    findings
        .iter()
        .map(|finding| packet(project, finding, questions(finding)))
        .collect()
}

fn packet(project: &mut Project, finding: &Finding, questions: Vec<String>) -> ReviewPacket {
    let span = project
        .enclosing_function(&finding.file, finding.line)
        .cloned();
    let function = span.as_ref().and_then(|span| {
        let (start, end) = window(span.start_line, span.end_line, finding.line);
        numbered(project, &finding.file, start, end, None)
    });
    let evidence = finding
        .evidence
        .iter()
        .filter_map(|evidence| {
            numbered(
                project,
                &evidence.file,
                evidence.line.saturating_sub(EVIDENCE_CONTEXT).max(1),
                evidence.line + EVIDENCE_CONTEXT,
                Some(evidence.note.clone()),
            )
        })
        .collect();
    let callers = span
        .as_ref()
        .map(|span| {
            let mut callers: Vec<CallerRef> = project
                .call_sites()
                .iter()
                .filter(|site| site.callee_id == span.id && !site.in_test)
                .map(|site| CallerRef {
                    name: site.caller.clone(),
                    file: site.file.clone(),
                    line: site.line,
                })
                .collect();
            callers.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
            callers.dedup_by(|a, b| a.name == b.name && a.file == b.file);
            callers.truncate(MAX_CALLERS);
            callers
        })
        .unwrap_or_default();
    ReviewPacket {
        finding: finding.clone(),
        function,
        evidence,
        callers,
        checklist: questions
            .into_iter()
            .chain(checklist(&finding.rule).into_iter().map(str::to_string))
            .collect(),
        confirmed_by: Vec::new(),
    }
}

/// The lines to quote of a function spanning `start..=end` for a finding at
/// `line`: all of it, or a window centred on the finding.
fn window(start: u32, end: u32, line: u32) -> (u32, u32) {
    let end = end.max(start);
    if end - start < MAX_FUNCTION_LINES {
        return (start, end);
    }
    let half = MAX_FUNCTION_LINES / 2;
    let from = line.saturating_sub(half).max(start);
    (from, (from + MAX_FUNCTION_LINES).min(end))
}

fn numbered(
    project: &mut Project,
    file: &str,
    start: u32,
    end: u32,
    note: Option<String>,
) -> Option<Snippet> {
    let parsed = project.parsed(file)?;
    let lines: Vec<&str> = parsed.source.lines().collect();
    let end = end.min(lines.len() as u32);
    if start == 0 || start > end {
        return None;
    }
    let source = (start..=end)
        .map(|n| format!("{n}\t{}", lines[n as usize - 1]))
        .collect::<Vec<_>>()
        .join("\n");
    Some(Snippet {
        file: file.to_string(),
        start_line: start,
        end_line: end,
        source,
        note,
    })
}

/// The questions a rule raises. Every packet also asks the general ones.
fn checklist(rule: &str) -> Vec<&'static str> {
    let specific: &[&'static str] = match rule {
        "arm-result-deviance" => &[
            "This arm does project work but its value is a constant, while the sibling arms return what their call produced. What should this arm report to the caller?",
            "If this arm's outcome is not recorded (state, manifest, return value), does the same condition come back on the next run — does the process converge?",
            "Is the constant deliberate (nothing to record), and is that stated?",
        ],
        "result-discarded" => &[
            "Other call sites use this function's result; here it is dropped. What does the result carry (an identity, a handle, a status) and is it needed after this call?",
            "Does anything downstream assume the result was recorded?",
        ],
        "missing-companion-call" => &[
            "Most callers of the first function also call the companion. Is the companion (cleanup, commit, record, release) needed here, on every path?",
            "If it is intentionally absent, is there an equivalent elsewhere in this function?",
        ],
        "loop-no-progress" => &[
            "Can anything in the loop body change the condition? If not, can the loop end?",
            "Is the condition changed by another thread, a callback or a borrow the syntax hides?",
        ],
        "identical-branches" => &[
            "Two branches do the same thing. Was one meant to differ (copy-paste), or can they be merged?",
        ],
        "self-comparison" => &["A value is compared with itself. Which operand was meant?"],
        "constant-condition" => {
            &["The condition cannot change. Is the branch dead or a leftover debug switch?"]
        }
        "dead-store" => &[
            "The first value is overwritten before it is read. Was it meant to be used, or is the first assignment dead?",
        ],
        miri if miri.starts_with("miri::") => &[
            "Miri proved this UB on a real execution. Which invariant did the unsafe code assume that this input broke (length, initialization, alignment, aliasing, lifetime)?",
            "Is the function sound for every input a safe caller can pass? If not, check the input or make the function `unsafe` with the contract documented.",
            "Rerun the same test on the fix (`codegraph analyze miri --finding <file:line>`): it must come back clean.",
        ],
        _ => &[],
    };
    let mut list = specific.to_vec();
    list.extend([
        "Termination: does every loop or retry end, and does repeated execution converge?",
        "Error paths: is every failure handled or reported, and is state left consistent?",
        "Confirm by a test that fails before a fix and passes after it.",
    ]);
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_functions_are_quoted_whole_and_long_ones_windowed() {
        assert_eq!(window(10, 40, 20), (10, 40));
        let (from, to) = window(1, 1000, 500);
        assert!(from <= 500 && 500 <= to);
        assert_eq!(to - from, MAX_FUNCTION_LINES);
        let (from, _) = window(1, 1000, 3);
        assert_eq!(from, 1);
    }

    #[test]
    fn every_checklist_ends_with_the_general_questions() {
        for rule in ["arm-result-deviance", "dead-store", "unknown-rule"] {
            let list = checklist(rule);
            assert!(list.last().unwrap().starts_with("Confirm by a test"));
        }
        assert!(checklist("arm-result-deviance")[1].contains("converge"));
    }
}

//! `codegraph analyze rules`: bug rules written as YAML — by a person or a
//! model — instead of detectors compiled into the binary, matched on the
//! syntax tree and checked against the index.
//!
//! A rule (format after weggli-ruleset) has an `id`, `severity`, `tags`, a
//! `message`, and `check-patterns`: any match of one is a finding, unless an
//! `ignore-patterns` match covers it. A pattern is either
//!
//! - a **weggli** `pattern` (C/C++; [`weggli`], ported from weggli-rs):
//!   code with `$var` variables, `_` wildcards, `not:` negative statements,
//!   `strict:`, `_(e)` sub-expressions, plus `regex: [var=re, var!=re]`,
//!   `unique` and `limit`; or
//! - a tree-sitter **`query`** for any language with a grammar, with the
//!   text predicates tree-sitter evaluates (`#eq?`, `#match?`, `#any-of?`…).
//!
//! and `where` predicates that read what the syntax cannot show
//! ([`semantics`]): `resolves-to` (the call at a capture resolves, in the
//! index, to a callee whose qualified name matches), `enclosing-function`
//! (`calls`/`calls-not` a resolved callee, `name-regex`, `is-test`),
//! `inside`/`not-inside` (a structural ancestor), and capture
//! `regex`/`not-regex`.
//!
//! A rule may instead be a **taint** rule (`taint:` with `sources`,
//! `sinks`, `sanitizers`, `propagators`, each a list of such patterns plus
//! the capture its role reads): a finding is a sink whose value a source's
//! value reaches in the same function ([`taint`]), with the path as
//! evidence.
//!
//! Every rule carries `examples` (`bad` code it must match, `good` code it
//! must not); `--check` runs them without an index, so a rule can be tried
//! in many permutations in seconds. Findings are
//! [`Finding`]s of [`Detector::Rule`], ranked and reviewed
//! (`analyze review --rules`) like the built-in detectors'.

mod builtin;
mod check;
mod compile;
mod engine;
mod lang;
mod locate;
mod semantics;
mod spec;
mod taint;
#[cfg(test)]
mod tests;
pub mod weggli;

use std::path::{Path, PathBuf};

pub use builtin::BUILTIN_RULES;
pub use check::{CheckReport, ExampleCheck, RuleCheck, check_rules};
pub use compile::{LoadError, Rule, RuleSet};
use engine::{FileInput, one_line, position, render_message};
use semantics::IndexSemantics;
pub use spec::Severity;

use super::bugs::{self, BugsOptions, BugsReport, Detector, Evidence, Finding, Project};
use crate::codegraph::CodeGraph;
use crate::extraction::{create_parser, detect_language};

/// Files larger than this are bundles or generated code in practice.
const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;
/// Other captures listed as evidence of a finding.
const MAX_CAPTURE_EVIDENCE: usize = 4;
/// Steps of a taint flow listed as evidence (between source and sink).
const MAX_TAINT_HOPS: usize = 8;

impl RuleSet {
    /// Rules from files and directories (`*.yml`/`*.yaml`, recursively,
    /// in path order), inline texts (`(label, yaml)`), and the built-in
    /// rules. Problems are collected in [`RuleSet::errors`], not raised.
    pub fn load(paths: &[PathBuf], texts: &[(String, String)], builtin: bool) -> RuleSet {
        let mut set = RuleSet::default();
        if builtin {
            for (name, text) in BUILTIN_RULES {
                set.add_text(&format!("builtin:{name}"), text);
            }
        }
        for path in paths {
            set.add_path(path);
        }
        for (label, text) in texts {
            set.add_text(label, text);
        }
        set
    }

    fn add_path(&mut self, path: &Path) {
        let mut files = Vec::new();
        if path.is_dir() {
            for entry in walkdir::WalkDir::new(path)
                .sort_by_file_name()
                .into_iter()
                .filter_map(Result::ok)
            {
                let is_yaml = entry
                    .path()
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext == "yml" || ext == "yaml");
                if entry.file_type().is_file() && is_yaml {
                    files.push(entry.into_path());
                }
            }
            if files.is_empty() {
                self.errors.push(LoadError {
                    source: path.display().to_string(),
                    line: None,
                    rule: None,
                    message: "no .yml/.yaml rule files in this directory".to_string(),
                });
            }
        } else {
            files.push(path.to_path_buf());
        }
        for file in files {
            let source = file.display().to_string();
            match std::fs::read_to_string(&file) {
                Ok(text) => self.add_text(&source, &text),
                Err(e) => self.errors.push(LoadError {
                    source,
                    line: None,
                    rule: None,
                    message: format!("cannot read: {e}"),
                }),
            }
        }
    }

    /// The rule named `id`.
    pub fn get(&self, id: &str) -> Option<&Rule> {
        self.rules.iter().find(|rule| rule.id == id)
    }
}

/// The review questions of a rule finding: its description and `review`.
pub fn review_questions(rules: &RuleSet, finding: &Finding) -> Vec<String> {
    if finding.detector != Detector::Rule {
        return Vec::new();
    }
    let Some(rule) = rules.get(&finding.rule) else {
        return Vec::new();
    };
    let mut questions = Vec::new();
    if let Some(description) = &rule.description {
        questions.push(format!(
            "Rule `{}` ({}): {}",
            rule.id,
            rule.severity.as_str(),
            one_line(description, 400)
        ));
    }
    questions.extend(rule.review.iter().cloned());
    questions
}

/// Run `rules` over the project indexed at `project_root`.
pub fn rules_report(
    cg: &CodeGraph,
    project_root: &Path,
    rules: &RuleSet,
    options: &BugsOptions,
) -> Result<BugsReport, String> {
    let mut project = Project::load(cg, project_root)?;
    let (mut findings, scanned) = run(cg, &mut project, rules, options)?;
    bugs::rank(&mut findings);
    Ok(bugs::report(
        &project,
        scanned,
        findings,
        "Rule findings are matches of YAML rules (syntax patterns with semantic predicates \
         read from the index). Confirm each by reading the code (`codegraph analyze review \
         --rules …`); a rule that misfires is fixed by editing its patterns and examples and \
         re-running `codegraph analyze rules --check`.",
    ))
}

/// Every finding of `rules` in the project, filtered by `options` (not
/// ranked).
pub(crate) fn detect(
    cg: &CodeGraph,
    project: &mut Project,
    rules: &RuleSet,
    options: &BugsOptions,
) -> Result<Vec<Finding>, String> {
    run(cg, project, rules, options).map(|(findings, _)| findings)
}

/// [`detect`], and how many files were matched.
fn run(
    cg: &CodeGraph,
    project: &mut Project,
    rules: &RuleSet,
    options: &BugsOptions,
) -> Result<(Vec<Finding>, usize), String> {
    if rules.rules.is_empty() {
        return Ok((Vec::new(), 0));
    }
    let scan = {
        let semantics = IndexSemantics::new(cg, project)?;
        scan(project, &semantics, rules, options)
    };
    for reason in scan.skipped {
        project.skip(&reason);
    }
    let mut findings = scan.findings;
    bugs::retain_selected(project, &mut findings, options);
    Ok((findings, scan.scanned))
}

/// What one pass over the project's files found.
struct Scan {
    findings: Vec<Finding>,
    scanned: usize,
    skipped: Vec<String>,
}

/// Run `rules` over every file of `project` they apply to (`only_files`
/// honoured; test filtering is the caller's).
fn scan(
    project: &Project,
    semantics: &IndexSemantics,
    rules: &RuleSet,
    options: &BugsOptions,
) -> Scan {
    let mut out = Scan {
        findings: Vec::new(),
        scanned: 0,
        skipped: Vec::new(),
    };
    let wanted = |file: &String| {
        options
            .only_files
            .as_ref()
            .is_none_or(|only| only.iter().any(|f| f == file))
    };
    for file in project.files().iter().filter(|file| wanted(file)) {
        let language = detect_language(file, None);
        if !rules.rules.iter().any(|rule| rule.runs_on(language)) {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(project.root().join(file)) else {
            out.skipped.push("unreadable".to_string());
            continue;
        };
        if source.len() > MAX_SOURCE_BYTES {
            out.skipped.push("over 2 MiB".to_string());
            continue;
        }
        // `.h` files: C or C++ by their content.
        let language = detect_language(file, Some(&source));
        let applicable: Vec<&Rule> = rules
            .rules
            .iter()
            .filter(|rule| rule.runs_on(language))
            .collect();
        if applicable.is_empty() {
            continue;
        }
        let Some(tree) = create_parser(language).and_then(|mut p| p.parse(&source, None)) else {
            out.skipped
                .push(format!("no parser ({})", language.as_str()));
            continue;
        };
        out.scanned += 1;
        let input = FileInput::new(file, language, &source, &tree);
        for rule in applicable {
            for hit in &engine::run_rule(rule, &input, semantics, false).hits {
                out.findings.push(finding(rule, hit, &input, semantics));
            }
        }
    }
    out
}

fn finding(
    rule: &Rule,
    hit: &engine::Hit,
    file: &FileInput,
    semantics: &IndexSemantics,
) -> Finding {
    let pattern = &rule.checks[hit.pattern];
    let (line, col) = position(file.tree, hit.at.start);
    let function = semantics.function_at(file.path, line);
    let template = pattern
        .message
        .as_deref()
        .or(rule.message.as_deref())
        .map(str::to_string)
        .or_else(|| {
            rule.description
                .as_deref()
                .and_then(|d| d.lines().next())
                .map(str::to_string)
        })
        .unwrap_or_else(|| format!("matches {}", pattern.label));
    let mut message = render_message(
        &template,
        hit,
        file.source,
        function.map(|f| f.name.as_str()),
    );
    if rule.checks.len() > 1 && !pattern.name.contains('[') {
        message.push_str(&format!(" [{}]", pattern.name));
    }

    let mut evidence: Vec<Evidence> = hit
        .notes
        .iter()
        .map(|note| Evidence {
            file: file.path.to_string(),
            line,
            note: note.clone(),
        })
        .collect();
    if let Some(flow) = &hit.flow {
        evidence.push(Evidence {
            file: file.path.to_string(),
            line: flow.source_line,
            note: format!("source: `{}` ({})", flow.source_code, flow.source_pattern),
        });
        for &hop in flow.hops.iter().take(MAX_TAINT_HOPS) {
            evidence.push(Evidence {
                file: file.path.to_string(),
                line: hop,
                note: format!("flows through `{}`", one_line(file.line_text(hop), 80)),
            });
        }
        evidence.push(Evidence {
            file: file.path.to_string(),
            line,
            note: format!("sink: `{}`", one_line(file.line_text(line), 80)),
        });
        return Finding {
            detector: Detector::Rule,
            rule: rule.id.clone().into(),
            file: file.path.to_string(),
            line,
            col,
            function: function.map(|f| f.qualified_name.clone()),
            message,
            confidence: rule.confidence,
            evidence,
        };
    }
    for (name, range) in &hit.captures {
        if evidence.len() >= MAX_CAPTURE_EVIDENCE + hit.notes.len() {
            break;
        }
        let (capture_line, _) = position(file.tree, range.start);
        if capture_line == line {
            continue;
        }
        evidence.push(Evidence {
            file: file.path.to_string(),
            line: capture_line,
            note: format!("`{name}` = {}", one_line(&file.source[range.clone()], 80)),
        });
    }

    Finding {
        detector: Detector::Rule,
        rule: rule.id.clone().into(),
        file: file.path.to_string(),
        line,
        col,
        function: function.map(|f| f.qualified_name.clone()),
        message,
        confidence: rule.confidence,
        evidence,
    }
}

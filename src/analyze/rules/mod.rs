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
//! `sinks`, `sanitizers`, `propagators` and `guards`, each a list of such
//! patterns plus the capture its role reads): a finding is a sink whose
//! value a source's value reaches — in one function or across the
//! project's calls and files ([`taint`]) — with every step as evidence.
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
mod saved;
pub mod score;
mod semantics;
mod spec;
mod taint;
#[cfg(test)]
mod tests;
mod variant;
pub mod weggli;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub use builtin::BUILTIN_RULES;
pub use check::{CheckReport, ExampleCheck, RuleCheck, check_rules};
use codegraph_analysis::taint_flow::Budget;
pub use compile::{LoadError, Rule, RuleSet};
use engine::{FileInput, one_line, position, render_message};
pub use saved::{SaveError, SaveOutcome, rules_dir, save_rule, saved_rule_texts};
pub use score::{ScoreOptions, ScoreReport, score_rules};
use semantics::IndexSemantics;
pub use spec::Severity;
pub use variant::{
    SkeletonCheck,
    VariantCall,
    VariantFunction,
    VariantNode,
    VariantReport,
    variant,
};

use super::bugs::{self, BugsOptions, BugsReport, Detector, Evidence, Finding, Project};
use crate::codegraph::CodeGraph;
use crate::extraction::{create_parser, detect_language};
use crate::types::Language;

/// Files larger than this are bundles or generated code in practice.
const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;
/// Other captures listed as evidence of a finding.
const MAX_CAPTURE_EVIDENCE: usize = 4;
/// Steps of a taint flow listed as evidence (between source and sink).
const MAX_TAINT_HOPS: usize = 12;
/// Source bytes one sweep keeps parsed for the taint pass, at most.
const MAX_TAINT_SOURCE_BYTES: usize = 32 * 1024 * 1024;
/// Time the taint pass may take when the caller sets none.
const DEFAULT_TAINT_BUDGET: Duration = Duration::from_secs(60);
/// A flow through a guessed call target is this much less certain.
const GUESSED_CONFIDENCE: f64 = 0.8;

/// The `(label, yaml)` of rule files and directories (`*.yml`/`*.yaml`,
/// recursively, in path order), inline texts, and the built-in rules — what
/// [`RuleSet::load`] compiles, and what a score's cache is keyed on. Files
/// that cannot be read are returned as errors.
pub fn collect_rule_texts(
    paths: &[PathBuf],
    texts: &[(String, String)],
    builtin: bool,
) -> (Vec<(String, String)>, Vec<LoadError>) {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    if builtin {
        for (name, text) in BUILTIN_RULES {
            out.push((format!("builtin:{name}"), text.to_string()));
        }
    }
    for path in paths {
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
                errors.push(LoadError {
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
                Ok(text) => out.push((source, text)),
                Err(e) => errors.push(LoadError {
                    source,
                    line: None,
                    rule: None,
                    message: format!("cannot read: {e}"),
                }),
            }
        }
    }
    out.extend(texts.iter().cloned());
    (out, errors)
}

impl RuleSet {
    /// Rules from files and directories (`*.yml`/`*.yaml`, recursively,
    /// in path order), inline texts (`(label, yaml)`), and the built-in
    /// rules. Problems are collected in [`RuleSet::errors`], not raised.
    pub fn load(paths: &[PathBuf], texts: &[(String, String)], builtin: bool) -> RuleSet {
        let (texts, errors) = collect_rule_texts(paths, texts, builtin);
        let mut set = RuleSet {
            errors,
            ..RuleSet::default()
        };
        for (label, text) in &texts {
            set.add_text(label, text);
        }
        set
    }

    /// Add the rules of `texts` whose id is not loaded yet: a rule passed
    /// explicitly shadows a saved one of the same id (the edited version
    /// runs, not both). Returns how many were added.
    pub fn add_shadowed(&mut self, texts: &[(String, String)]) -> usize {
        let mut more = RuleSet::default();
        for (label, text) in texts {
            more.add_text(label, text);
        }
        let before = self.rules.len();
        for rule in more.rules {
            if self.get(&rule.id).is_none() {
                self.rules.push(rule);
            }
        }
        self.errors.extend(more.errors);
        self.rules.len() - before
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
/// honoured; test filtering is the caller's). Check rules run file by
/// file; taint rules run once over every file they apply to, together, so
/// flows cross files (their findings are narrowed to `only_files` later).
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
    let taint_rules: Vec<(&Rule, &compile::TaintRule)> = rules
        .rules
        .iter()
        .filter_map(|rule| rule.taint.as_ref().map(|taint| (rule, taint)))
        .collect();
    // Files taint rules apply to, kept for the project-wide pass.
    let mut kept: Vec<(String, Language, String, tree_sitter::Tree)> = Vec::new();
    let mut kept_bytes = 0usize;
    for file in project.files() {
        let is_wanted = wanted(file);
        let language = detect_language(file, None);
        let runs = |rule: &&Rule| rule.runs_on(language) && (is_wanted || rule.taint.is_some());
        if !rules.rules.iter().any(|rule| runs(&rule)) {
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
        if is_wanted {
            out.scanned += 1;
            let input = FileInput::new(file, language, &source, &tree);
            for rule in applicable.iter().filter(|rule| rule.taint.is_none()) {
                for hit in &engine::run_rule(rule, &input, semantics, false).hits {
                    out.findings.push(finding(rule, hit, &input, semantics));
                }
            }
        }
        if applicable.iter().any(|rule| rule.taint.is_some()) {
            if kept_bytes + source.len() > MAX_TAINT_SOURCE_BYTES {
                out.skipped
                    .push("taint: over the source budget of one sweep".to_string());
                continue;
            }
            kept_bytes += source.len();
            kept.push((file.clone(), language, source, tree));
        }
    }
    if kept.is_empty() || taint_rules.is_empty() {
        return out;
    }
    let inputs: Vec<FileInput> = kept
        .iter()
        .map(|(path, language, source, tree)| FileInput::new(path, *language, source, tree))
        .collect();
    let refs: Vec<&FileInput> = inputs.iter().collect();
    let deadline = Instant::now() + options.taint_budget.unwrap_or(DEFAULT_TAINT_BUDGET);
    let mut budget = Budget::new(taint::MAX_STEPS, Some(deadline));
    let outcome = taint::run_files(
        &taint_rules,
        &refs,
        semantics,
        &mut budget,
        taint::MAX_PROGRAM_OPS,
        false,
    );
    if outcome.partial {
        out.skipped.push(
            "taint: the budget ran out, so some flows across functions were not followed"
                .to_string(),
        );
    }
    for ((rule, _), per_file) in taint_rules.iter().zip(outcome.results) {
        for (input, result) in inputs.iter().zip(per_file) {
            if result.hits.is_empty() {
                continue;
            }
            let result = engine::drop_ignored(rule, input, semantics, result, false);
            for hit in &result.hits {
                out.findings.push(finding(rule, hit, input, semantics));
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
            file: flow.source_file.clone(),
            line: flow.source_line,
            note: format!("source: `{}` ({})", flow.source_code, flow.source_pattern),
        });
        for hop in flow.hops.iter().take(MAX_TAINT_HOPS) {
            evidence.push(Evidence {
                file: hop.file.clone(),
                line: hop.line,
                note: format!("flows through `{}`", hop.code),
            });
        }
        if flow.hops.len() > MAX_TAINT_HOPS {
            evidence.push(Evidence {
                file: file.path.to_string(),
                line,
                note: format!("… {} more steps", flow.hops.len() - MAX_TAINT_HOPS),
            });
        }
        if flow.guessed {
            evidence.push(Evidence {
                file: file.path.to_string(),
                line,
                note: "crosses a call whose target was inferred from the syntax, not an index \
                       edge"
                    .to_string(),
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
            confidence: if flow.guessed {
                rule.confidence * GUESSED_CONFIDENCE
            } else {
                rule.confidence
            },
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

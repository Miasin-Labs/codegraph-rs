//! codegraph_rules — the loop a model writes bug rules in (opt-in through
//! `CODEGRAPH_MCP_TOOLS`): `check` a rule's examples (no index), `run` rules
//! over the project, `variant` a found bug into the material for a rule
//! that sweeps for its variants, `save` a passing rule to
//! `.codegraph/rules/` (every later sweep runs it), and `score` rules on a
//! labeled corpus to keep them only when they beat the base rate.
//!
//! Not read-only: `save` writes the project's rules directory and `score`
//! stages and indexes corpus units in a work directory. `score` is bounded
//! by `wait` and resumes from its `nextCursor`; the rest are bounded by the
//! project (rules run in-process over the index, like `analyze rules`).

mod output;

use std::path::PathBuf;
use std::time::Duration;

pub(in crate::mcp::tools) use output::rules_output_schema;
use output::{
    CheckExample,
    CheckOutput,
    CheckRule,
    EvidenceRow,
    FindingRow,
    FunctionOut,
    RunOutput,
    SaveOutput,
    ScoreOutput,
    VariantFinding,
    VariantOutput,
};
use serde_json::{Map, Value};

use super::context::ToolHandler;
use super::format::{mcp_output_budget, num_or};
use super::output::{Trim, fitted};
use super::schema::ToolResult;
use crate::analyze::bugs::{BugsOptions, Detector};
use crate::analyze::rules::{
    self,
    RuleSet,
    SaveError,
    ScoreOptions,
    check_rules,
    collect_rule_texts,
    rules_report,
    save_rule,
    saved_rule_texts,
    score_rules,
};
use crate::error::Result;
use crate::mcp::explore_session::{ProjectState, SESSION_ARG, range_already_sent};
use crate::utils::clamp;

/// Time a `run`'s taint pass may take.
const RUN_TAINT_BUDGET: std::time::Duration = std::time::Duration::from_secs(20);

/// Default and longest `wait` of a score, seconds.
const SCORE_WAIT: f64 = 25.0;
const SCORE_WAIT_MAX: f64 = 55.0;

impl ToolHandler {
    pub(in crate::mcp::tools) fn handle_rules(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolResult> {
        match args.get("action").and_then(Value::as_str) {
            Some("check") => self.rules_check(args),
            Some("run") => self.rules_run(args),
            Some("variant") => self.rules_variant(args),
            Some("save") => self.rules_save(args),
            Some("score") => self.rules_score(args),
            other => Ok(self.validation_error_result(
                "action",
                &format!(
                    "unknown action {}",
                    other.map_or("(none)".to_string(), |a| format!("`{a}`"))
                ),
                "one of check, run, variant, save, score",
                Some("string"),
            )),
        }
    }

    /// The project root, if the call names or has an indexed project.
    fn rules_root(&self, args: &Map<String, Value>) -> Result<PathBuf> {
        let cg = self.get_code_graph(args.get("projectPath").and_then(Value::as_str))?;
        Ok(cg.get_project_root().to_path_buf())
    }

    /// `yaml` and the built-in rules, as `(label, text)`.
    fn rule_texts(args: &Map<String, Value>) -> Vec<(String, String)> {
        let yaml: Vec<(String, String)> = args
            .get("yaml")
            .and_then(Value::as_str)
            .filter(|y| !y.trim().is_empty())
            .map(|y| vec![("<yaml>".to_string(), y.to_string())])
            .unwrap_or_default();
        let builtin = args.get("builtin").and_then(Value::as_bool) == Some(true);
        collect_rule_texts(&[], &yaml, builtin).0
    }

    fn wants_saved(args: &Map<String, Value>) -> bool {
        args.get("saved").and_then(Value::as_bool) != Some(false)
    }

    fn rules_check(&self, args: &Map<String, Value>) -> Result<ToolResult> {
        let texts = Self::rule_texts(args);
        let mut rules = RuleSet::load(&[], &texts, false);
        if texts.is_empty() {
            // Nothing given: check the project's saved rules.
            if let Ok(root) = self.rules_root(args) {
                rules.add_saved(&root);
            }
            if rules.rules.is_empty() && rules.errors.is_empty() {
                return Ok(self.validation_error_result(
                    "yaml",
                    "no rules to check: pass `yaml` (or `builtin`), or save rules first",
                    "rule YAML",
                    None,
                ));
            }
        }
        let report = check_rules(&rules);
        let payload = CheckOutput {
            schema_version: 1,
            kind: "ruleCheck",
            passed: report.passed,
            failed: report.failed,
            rules: report
                .rules
                .iter()
                .map(|rule| CheckRule {
                    id: rule.id.clone(),
                    passed: rule.passed,
                    line: rule.line,
                    error: rule.error.clone(),
                    examples: rule
                        .examples
                        .iter()
                        .map(|example| CheckExample {
                            example: example.example.clone(),
                            passed: example.passed,
                            why: (!example.passed).then(|| example.explain()),
                        })
                        .collect(),
                })
                .collect(),
            errors: report.errors.clone(),
        };
        let text = format!(
            "{} passed, {} failed{}",
            report.passed,
            report.failed,
            report
                .rules
                .iter()
                .flat_map(|rule| {
                    rule.error.iter().cloned().chain(
                        rule.examples
                            .iter()
                            .filter(|e| !e.passed)
                            .map(|e| format!("{}: {}", rule.id, e.explain())),
                    )
                })
                .chain(report.errors.iter().cloned())
                .map(|line| format!("\n{line}"))
                .collect::<String>()
        );
        self.structured_result(&text, &payload)
    }

    fn rules_run(&self, args: &Map<String, Value>) -> Result<ToolResult> {
        let cg = self.get_code_graph(args.get("projectPath").and_then(Value::as_str))?;
        let root = cg.get_project_root().to_path_buf();
        let texts = Self::rule_texts(args);
        let mut rules = RuleSet::load(&[], &texts, false);
        if Self::wants_saved(args) {
            rules.add_saved(&root);
        }
        if !rules.errors.is_empty() {
            let list: Vec<String> = rules.errors.iter().map(ToString::to_string).collect();
            return Ok(self.error_result(&format!(
                "{} rule problem(s) — fix them (`check` explains each):\n{}",
                list.len(),
                list.join("\n")
            )));
        }
        if rules.rules.is_empty() {
            return Ok(self.validation_error_result(
                "yaml",
                "no rules to run: pass `yaml` or `builtin`, or save rules first",
                "rule YAML",
                None,
            ));
        }
        let options = BugsOptions {
            detectors: vec![Detector::Rule],
            only_files: None,
            include_tests: args.get("tests").and_then(Value::as_bool) == Some(true),
            // A request must answer: the taint pass stops here and says so.
            taint_budget: Some(RUN_TAINT_BUDGET),
        };
        let report = rules_report(&cg, &root, &rules, &options)
            .map_err(crate::error::CodeGraphError::other)?;
        let limit = clamp(num_or(args, "limit", 50.0), 1.0, 500.0) as usize;
        let total = report.findings.len();
        let findings: Vec<FindingRow> = report
            .findings
            .iter()
            .take(limit)
            .map(|finding| FindingRow {
                rule: finding.rule.to_string(),
                file: finding.file.clone(),
                line: finding.line,
                function: finding.function.clone(),
                message: finding.message.clone(),
                evidence: evidence_rows(&finding.file, &finding.evidence),
            })
            .collect();
        let payload = RunOutput {
            schema_version: 1,
            kind: "ruleRun",
            rules: rules.rules.len(),
            files_scanned: report.files_scanned,
            total,
            by_rule: report.by_rule.clone(),
            omitted: total - findings.len(),
            findings,
            truncated: false,
        };
        let text = format!(
            "{total} findings of {} rules over {} files",
            rules.rules.len(),
            report.files_scanned
        );
        let value = fitted(
            &payload,
            mcp_output_budget(),
            &[Trim::counted("findings", "omitted")],
        );
        self.structured_result(&text, &value)
    }

    fn rules_variant(&self, args: &Map<String, Value>) -> Result<ToolResult> {
        let Some((file, line)) = args
            .get("at")
            .and_then(Value::as_str)
            .and_then(|at| at.trim().rsplit_once(':'))
            .and_then(|(file, line)| Some((file.to_string(), line.trim().parse::<u32>().ok()?)))
        else {
            return Ok(self.validation_error_result(
                "at",
                "variant needs `at`: the bug's `file:line` (project-relative)",
                "file:line",
                Some("string"),
            ));
        };
        if file.contains("..") {
            return Ok(self.validation_error_result(
                "at",
                "`at` must be a project-relative path",
                "file:line",
                Some("string"),
            ));
        }
        let cg = self.get_code_graph(args.get("projectPath").and_then(Value::as_str))?;
        let root = cg.get_project_root().to_path_buf();
        let report = match rules::variant(&cg, &root, &file, line) {
            Ok(report) => report,
            Err(message) => return Ok(self.error_result(&message)),
        };
        // The session ledger: a window this conversation already holds is
        // not sent again.
        let prior = args
            .get(SESSION_ARG)
            .and_then(|value| serde_json::from_value::<ProjectState>(value.clone()).ok());
        let function = report.function.as_ref().map(|function| {
            let sent = prior.as_ref().is_some_and(|prior| {
                range_already_sent(
                    prior,
                    &root,
                    &report.file,
                    function.start_line as usize,
                    function.end_line as usize,
                )
            });
            FunctionOut {
                name: function.name.clone(),
                start_line: function.start_line,
                end_line: function.end_line,
                source: (!sent).then(|| function.source.clone()),
                already_sent: sent.then_some(true),
            }
        });
        let payload = VariantOutput {
            schema_version: 1,
            kind: "ruleVariant",
            file: report.file.clone(),
            line: report.line,
            language: report.language.clone(),
            function,
            node: report.node.clone(),
            skeleton: report.skeleton.clone(),
            skeleton_check: report.skeleton_check.clone(),
            calls: report.calls.clone(),
            findings: report
                .findings
                .iter()
                .map(|finding| VariantFinding {
                    rule: finding.rule.to_string(),
                    line: finding.line,
                    message: finding.message.clone(),
                    confidence: (finding.confidence * 100.0).round() / 100.0,
                    evidence: evidence_rows(&finding.file, &finding.evidence),
                })
                .collect(),
            truncated: false,
        };
        let text = format!(
            "{}:{} — `{}` ({} calls, {} findings); skeleton {}",
            report.file,
            report.line,
            report.node.kind,
            report.calls.len(),
            report.findings.len(),
            if report.skeleton_check.passed {
                "passes check"
            } else {
                "does not pass check yet"
            }
        );
        let value = fitted(
            &payload,
            mcp_output_budget(),
            &[Trim::plain("calls"), Trim::plain("findings")],
        );
        self.structured_result(&text, &value)
    }

    fn rules_save(&self, args: &Map<String, Value>) -> Result<ToolResult> {
        let Some(yaml) = args
            .get("yaml")
            .and_then(Value::as_str)
            .filter(|y| !y.trim().is_empty())
        else {
            return Ok(self.validation_error_result(
                "yaml",
                "save needs `yaml`: the rule to save",
                "rule YAML",
                Some("string"),
            ));
        };
        let root = self.rules_root(args)?;
        match save_rule(&root, yaml) {
            Ok(outcome) => {
                let text = format!(
                    "{} {} (every `analyze rules` sweep of this project runs it)",
                    if outcome.replaced {
                        "Replaced"
                    } else {
                        "Saved"
                    },
                    outcome.path
                );
                let saved = saved_rule_texts(&root).len();
                self.structured_result(
                    &text,
                    &SaveOutput {
                        schema_version: 1,
                        kind: "ruleSave",
                        id: outcome.id,
                        path: outcome.path,
                        replaced: outcome.replaced,
                        saved,
                    },
                )
            }
            Err(SaveError::Check(report)) => {
                let reasons: Vec<String> = report
                    .errors
                    .iter()
                    .cloned()
                    .chain(report.rules.iter().flat_map(|rule| {
                        rule.error.iter().cloned().chain(
                            rule.examples
                                .iter()
                                .filter(|e| !e.passed)
                                .map(|e| format!("{}: {}", rule.id, e.explain())),
                        )
                    }))
                    .collect();
                Ok(self.validation_error_result(
                    "yaml",
                    &format!(
                        "not saved: the rule does not pass check —\n{}",
                        reasons.join("\n")
                    ),
                    "a rule whose bad examples match and good examples do not",
                    None,
                ))
            }
            Err(SaveError::Invalid(message)) => Ok(self.validation_error_result(
                "yaml",
                &message,
                "one rule with a file-name id",
                None,
            )),
            Err(SaveError::Io(message)) => Ok(self.error_result(&format!("not saved: {message}"))),
        }
    }

    fn rules_score(&self, args: &Map<String, Value>) -> Result<ToolResult> {
        let Some(corpus) = args
            .get("corpus")
            .and_then(Value::as_str)
            .filter(|c| !c.trim().is_empty())
        else {
            return Ok(self.validation_error_result(
                "corpus",
                "score needs `corpus`: a bugbench corpus directory (with ground_truth.jsonl)",
                "directory path",
                Some("string"),
            ));
        };
        let texts = Self::rule_texts(args);
        let saved = if Self::wants_saved(args) {
            self.rules_root(args)
                .map(|root| saved_rule_texts(&root))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        if texts.is_empty() && saved.is_empty() {
            return Ok(self.validation_error_result(
                "yaml",
                "no rules to score: pass `yaml` or `builtin`, or save rules first",
                "rule YAML",
                None,
            ));
        }
        // Not `num_or`: `wait: 0` would become the default.
        let wait = args
            .get("wait")
            .and_then(Value::as_f64)
            .unwrap_or(SCORE_WAIT);
        let mut options = ScoreOptions::new(PathBuf::from(corpus));
        options.deadline = Some(Duration::from_secs_f64(clamp(wait, 1.0, SCORE_WAIT_MAX)));
        options.sample = args
            .get("sample")
            .and_then(Value::as_f64)
            .filter(|n| *n >= 1.0)
            .map(|n| n as usize);
        options.seed = args
            .get("seed")
            .and_then(Value::as_f64)
            .map_or(1, |n| n.max(0.0) as u64);
        options.cursor = args
            .get("cursor")
            .and_then(Value::as_str)
            .map(str::to_string);
        options.work = args.get("work").and_then(Value::as_str).map(PathBuf::from);
        // The server lives on: an index the deadline cuts finishes in the
        // background, and the next call (with `nextCursor`) scores it.
        options.detach = true;
        let report = match score_rules(&texts, &saved, &options, &|_| {}) {
            Ok(report) => report,
            Err(message) => return Ok(self.error_result(&message)),
        };
        let text = report
            .rules
            .iter()
            .map(|rule| format!("{} {}: {}", rule.verdict.as_str(), rule.id, rule.reason))
            .collect::<Vec<_>>()
            .join("\n");
        let payload = ScoreOutput::from_report(&report);
        let value = fitted(&payload, mcp_output_budget(), &[Trim::plain("rules")]);
        self.structured_result(&text, &value)
    }
}

/// Evidence rows, the file left out where it is the finding's.
fn evidence_rows(file: &str, evidence: &[crate::analyze::bugs::Evidence]) -> Vec<EvidenceRow> {
    evidence
        .iter()
        .map(|e| EvidenceRow {
            file: (e.file != file).then(|| e.file.clone()),
            line: e.line,
            note: e.note.clone(),
        })
        .collect()
}

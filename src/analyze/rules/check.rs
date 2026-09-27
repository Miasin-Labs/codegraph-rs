//! `codegraph analyze rules --check`: run each rule's `examples` with no
//! index — every `bad` example must match, no `good` one may — and say
//! exactly why one did not: which pattern matched where, or which `where`
//! predicate or ignore-pattern turned a match down. This is the loop a
//! model iterates a rule in.

use serde::Serialize;

use super::compile::{Example, Rule, RuleSet};
use super::engine::{self, FileInput, one_line, position};
use super::semantics::SyntaxSemantics;
use crate::extraction::create_parser;

/// Rejections listed per failing example.
const MAX_REJECTIONS: usize = 5;

/// A match an example produced.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExampleMatch {
    pub pattern: String,
    /// 1-based line in the example.
    pub line: u32,
    pub code: String,
}

/// A match a predicate or ignore-pattern turned down.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExampleRejection {
    pub pattern: String,
    pub line: u32,
    pub reason: String,
}

/// One example's outcome.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExampleCheck {
    /// `bad[0]`, `good[1]`.
    pub example: String,
    /// Line of the example in the rule file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    pub passed: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub matches: Vec<ExampleMatch>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rejected: Vec<ExampleRejection>,
    /// The example does not parse cleanly (it may still match).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub syntax_error: Option<String>,
}

impl ExampleCheck {
    /// One line saying why it failed.
    pub fn explain(&self) -> String {
        let bad = self.example.starts_with("bad");
        let mut text = if bad {
            format!("{} did not match", self.example)
        } else {
            let first = &self.matches[0];
            format!(
                "{} matched {} at example line {}: `{}`",
                self.example, first.pattern, first.line, first.code
            )
        };
        if bad {
            for rejection in &self.rejected {
                text.push_str(&format!(
                    "; {} matched at example line {} but {}",
                    rejection.pattern, rejection.line, rejection.reason
                ));
            }
            if self.rejected.is_empty() {
                text.push_str(" (no pattern matched its syntax)");
            }
        }
        if let Some(error) = &self.syntax_error {
            text.push_str(&format!(" [note: {error}]"));
        }
        text
    }
}

/// One rule's outcome.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleCheck {
    pub id: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    pub passed: bool,
    /// The rule did not load (with where and why).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<ExampleCheck>,
}

/// Every rule's outcome.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckReport {
    pub rules: Vec<RuleCheck>,
    pub passed: usize,
    pub failed: usize,
    /// Files that did not load at all.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

impl CheckReport {
    pub fn ok(&self) -> bool {
        self.failed == 0 && self.errors.is_empty()
    }
}

/// Load problems and example outcomes of every rule in `rules`.
pub fn check_rules(rules: &RuleSet) -> CheckReport {
    let mut checks: Vec<RuleCheck> = Vec::new();
    let mut errors = Vec::new();
    for error in &rules.errors {
        match &error.rule {
            Some(id) => checks.push(RuleCheck {
                id: id.clone(),
                source: error.source.clone(),
                line: error.line,
                passed: false,
                error: Some(error.to_string()),
                examples: Vec::new(),
            }),
            None => errors.push(error.to_string()),
        }
    }
    for rule in &rules.rules {
        let examples: Vec<ExampleCheck> = rule
            .examples
            .iter()
            .map(|example| check_example(rule, example))
            .collect();
        checks.push(RuleCheck {
            id: rule.id.clone(),
            source: rule.source.clone(),
            line: (rule.line > 0).then_some(rule.line),
            passed: examples.iter().all(|e| e.passed),
            error: None,
            examples,
        });
    }
    checks.sort_by(|a, b| (&a.source, a.line, &a.id).cmp(&(&b.source, b.line, &b.id)));
    let passed = checks.iter().filter(|c| c.passed).count();
    CheckReport {
        failed: checks.len() - passed,
        passed,
        rules: checks,
        errors,
    }
}

/// Run `rule` over one example.
pub(super) fn check_example(rule: &Rule, example: &Example) -> ExampleCheck {
    let mut check = ExampleCheck {
        example: example.label(),
        line: example.line,
        passed: false,
        matches: Vec::new(),
        rejected: Vec::new(),
        syntax_error: None,
    };
    let Some(tree) = create_parser(example.language).and_then(|mut p| p.parse(&example.code, None))
    else {
        check.syntax_error = Some(format!("no parser for {}", example.language.as_str()));
        return check;
    };
    if tree.root_node().has_error() {
        check.syntax_error = Some(first_syntax_error(&tree, &example.code));
    }
    let input = FileInput::new(&example.file, example.language, &example.code, &tree);
    let semantics = SyntaxSemantics {
        resolves: &example.resolves,
    };
    let result = engine::run_rule(rule, &input, &semantics, true);
    check.matches = result
        .hits
        .iter()
        .map(|hit| ExampleMatch {
            pattern: rule.checks[hit.pattern].label.clone(),
            line: position(&tree, hit.at.start).0,
            code: one_line(&example.code[hit.at.clone()], 80),
        })
        .collect();
    check.rejected = result
        .rejected
        .into_iter()
        .take(MAX_REJECTIONS)
        .map(|r| ExampleRejection {
            pattern: r.pattern,
            line: r.line,
            reason: r.reason,
        })
        .collect();
    check.passed = example.bad != check.matches.is_empty();
    if check.passed {
        check.rejected.clear();
    }
    check
}

fn first_syntax_error(tree: &tree_sitter::Tree, code: &str) -> String {
    let mut cursor = tree.root_node().walk();
    loop {
        let node = cursor.node();
        if node.is_error() || node.is_missing() {
            let at = node.start_position();
            let what = if node.is_missing() {
                format!("missing `{}`", node.kind())
            } else {
                format!("unexpected `{}`", one_line(&code[node.byte_range()], 40))
            };
            return format!(
                "the example has a syntax error: {what} at example line {}",
                at.row + 1
            );
        }
        if node.has_error() && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return "the example has a syntax error".to_string();
            }
        }
    }
}

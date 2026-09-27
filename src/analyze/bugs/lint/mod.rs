//! Lint: classic logic-bug shapes read from the syntax, one walk per file.
//!
//! Rules (stable ids), each conservative — a lint that floods is useless, so
//! every rule gives up wherever the syntax cannot show the bug for certain:
//!
//! - `loop-no-progress` ([`loops`]): a condition-controlled loop whose
//!   condition reads only local variables and field paths that nothing in
//!   the body can change, and whose body cannot leave the loop.
//! - `identical-branches` ([`branches`]): `if`/`else` (or `c ? x : x`) with
//!   the same code on both sides; `match`/`switch` arms with the same
//!   non-trivial body under different patterns.
//! - `self-comparison` ([`conditions`]): `x == x`, `a.b < a.b`, `p && p`,
//!   `x - x` over side-effect-free operands (`x != x` is the NaN idiom).
//! - `constant-condition` ([`conditions`]): an `if`/ternary condition made
//!   of literals only (`1 == 1`), or decided by a literal (`false && x`).
//! - `dead-store` ([`stores`]): a local assigned and assigned again, in
//!   straight-line code of one block, with no read in between.
//!
//! Per-language node kinds live in [`rules`]; the rule code never branches
//! on a language. Test code is filtered by the caller.

mod branches;
mod conditions;
mod loops;
mod rules;
mod stores;
mod syntax;
#[cfg(test)]
mod tests;

use std::collections::HashMap;

use tree_sitter::Node;

use self::rules::Rules;
use self::syntax::{Scopes, position, walk};
use super::{Detector, Evidence, Finding, Project};
use crate::extraction::detect_language;

/// Files larger than this are bundles or generated code in practice; their
/// findings are noise and their walks the slowest.
const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;

/// Precision class of each rule, from hand-checked findings on 14 real
/// projects (Go, Python, Java, TypeScript, Rust; 2026-09):
/// - self-comparison: the non-fixture findings were a real bug
///   (`newWidth < newWidth`), a likely one (`fileName || fileName`) and a
///   harmless `url && url`; the rest decompiler output and test inputs.
/// - dead-store: every sampled finding was a genuine lost value; about a
///   third were bugs (Go errors dropped by `x, err :=` twice, a URL rewrite
///   lost), the rest redundant work.
/// - loop-no-progress: rare by construction; the one real-code finding was a
///   deliberate endless animation (`for loop {}` never clears `loop`).
/// - identical-branches: `if`/`else` twins are almost always redundant
///   rather than wrong; arms that pass the slip test were one real bug
///   (`Provisioned => …PROVISIONING`), three deliberate-looking aliases and
///   two intentional mappings in seven.
/// - constant-condition: every finding was a deliberate debug switch
///   (`false && …`, `x || true`) — worth a look, rarely a bug.
pub(super) const LOOP_NO_PROGRESS: f64 = 0.6;
pub(super) const IDENTICAL_BRANCHES: f64 = 0.4;
pub(super) const IDENTICAL_ARMS: f64 = 0.5;
pub(super) const SELF_COMPARISON: f64 = 0.8;
pub(super) const CONSTANT_CONDITION: f64 = 0.3;
pub(super) const DEAD_STORE: f64 = 0.6;

/// Tokens back on one line: spaces between words and around operators.
fn spaced(tokens: &[&str]) -> String {
    const OPERATORS: &[&str] = &[
        "=", "==", "!=", "===", "!==", "&&", "||", "+=", "-=", "*=", "/=", "|=", "&=", "=>", "->",
        "<", ">", "<=", ">=", "+", "-", "*", "/", "%", "?", ":", "|", "and", "or", "if", "else",
    ];
    let word_end = |t: &str| t.ends_with(|c: char| c.is_alphanumeric() || "_\"'`)]".contains(c));
    let word_start = |t: &str| t.starts_with(|c: char| c.is_alphanumeric() || "_\"'`".contains(c));
    let mut out = String::new();
    let mut prev: Option<&str> = None;
    for token in tokens {
        if let Some(p) = prev {
            let space = (word_end(p) && word_start(token))
                || OPERATORS.contains(token)
                || OPERATORS.contains(&p)
                || matches!(p, "," | ";" | "{")
                || *token == "}";
            if space {
                out.push(' ');
            }
        }
        out.push_str(token);
        prev = Some(token);
    }
    out
}

/// Bundled or minified code: over [`MAX_SOURCE_BYTES`], or lines averaging
/// over 300 bytes. Its findings are the minifier's, not the author's.
fn is_bundle(source: &str) -> bool {
    let lines = source.bytes().filter(|&b| b == b'\n').count() + 1;
    source.len() > MAX_SOURCE_BYTES || (source.len() > 4096 && source.len() / lines > 300)
}

/// A finding before the enclosing function is known.
struct Raw {
    rule: &'static str,
    line: u32,
    col: u32,
    message: String,
    confidence: f64,
    evidence: Vec<(u32, String)>,
}

/// One file's walk.
pub(super) struct Ctx<'s> {
    rules: &'static Rules,
    source: &'s str,
    scopes: Scopes,
    out: Vec<Raw>,
    /// Whether the project defines a symbol of this name.
    symbols: &'s dyn Fn(&str) -> bool,
    /// How often each word occurs in the file, built on first use.
    file_words: Option<HashMap<&'s str, usize>>,
}

impl<'s> Ctx<'s> {
    /// Whether `name` exists other than in the `local` occurrences the
    /// caller already accounts for: a symbol of the index (when `local` is
    /// 0), or more occurrences in this file than `local`.
    fn known(&mut self, name: &str, local: usize) -> bool {
        if local == 0 && (self.symbols)(name) {
            return true;
        }
        let source = self.source;
        let words = self.file_words.get_or_insert_with(|| {
            let mut counts = HashMap::new();
            for word in syntax::split_words(source) {
                *counts.entry(word).or_default() += 1;
            }
            counts
        });
        words.get(name).copied().unwrap_or(0) > local
    }

    fn report(
        &mut self,
        rule: &'static str,
        at: Node<'_>,
        confidence: f64,
        message: String,
        evidence: Vec<(u32, String)>,
    ) {
        let (line, col) = position(at);
        self.out.push(Raw {
            rule,
            line,
            col,
            message,
            confidence,
            evidence,
        });
    }

    /// Code shown in a message: its tokens on one line (comments dropped),
    /// bounded.
    fn snippet(&self, node: Node<'_>) -> String {
        let flat = spaced(&syntax::tokens(node, self.rules, self.source));
        if flat.chars().count() > 60 {
            let cut: String = flat.chars().take(57).collect();
            format!("{cut}…")
        } else {
            flat
        }
    }
}

pub(super) fn detect(project: &mut Project) -> Vec<Finding> {
    let mut findings = Vec::new();
    let files = project.files().to_vec();
    for file in files {
        let language = detect_language(&file, None);
        let Some(rules) = rules::for_language(language) else {
            continue;
        };
        if project.parsed(&file).is_none() {
            continue;
        }
        let symbols = |name: &str| project.has_symbol_named(name);
        let raws = match project.parsed_cached(&file) {
            Some(parsed) if !is_bundle(&parsed.source) => {
                lint_source(rules, &parsed.source, parsed.tree.root_node(), &symbols)
            }
            Some(_) => {
                project.skip("lint: bundled or minified");
                continue;
            }
            None => continue,
        };
        for raw in raws {
            let function = project
                .enclosing_function(&file, raw.line)
                .map(|span| span.qualified_name.clone());
            findings.push(Finding {
                detector: Detector::Lint,
                rule: raw.rule,
                file: file.clone(),
                line: raw.line,
                col: raw.col,
                function,
                message: raw.message,
                confidence: raw.confidence,
                evidence: raw
                    .evidence
                    .into_iter()
                    .map(|(line, note)| Evidence {
                        file: file.clone(),
                        line,
                        note,
                    })
                    .collect(),
            });
        }
    }
    findings
}

/// Every rule over one parsed file, in one walk.
fn lint_source(
    rules: &'static Rules,
    source: &str,
    root: Node<'_>,
    symbols: &dyn Fn(&str) -> bool,
) -> Vec<Raw> {
    let mut ctx = Ctx {
        rules,
        source,
        scopes: Scopes::default(),
        out: Vec::new(),
        symbols,
        file_words: None,
    };
    walk(root, |node| {
        let kind = node.kind();
        if rules.comments.contains(&kind) {
            return false;
        }
        if rules.cond_loops.iter().any(|l| l.kind == kind) {
            loops::check(node, &mut ctx);
        }
        if rules.ifs.iter().any(|shape| shape.kind == kind) {
            branches::check_if(node, &mut ctx);
        }
        if rules.ternaries.iter().any(|t| t.kind == kind) {
            branches::check_ternary(node, &mut ctx);
        }
        if rules.switches.iter().any(|s| s.kind == kind) {
            branches::check_switch(node, &mut ctx);
        }
        if rules.binaries.iter().any(|b| b.kind == kind) {
            conditions::check_self_comparison(node, &mut ctx);
        }
        conditions::check_constant_condition(node, &mut ctx);
        if rules.blocks.contains(&kind) {
            stores::check_block(node, &mut ctx);
        }
        true
    });
    ctx.out
}

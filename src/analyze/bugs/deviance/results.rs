//! `result-discarded`: a function whose result most of its call sites use,
//! called where the result is thrown away.
//!
//! Each resolved call is classified from its syntactic context: bound to a
//! name, passed on, returned, tested, chained — used; an expression
//! statement, `let _ =`, `_ =`, `void f()`, a `match`/`if let` whose success
//! patterns bind nothing (`Ok(_)`), `.is_ok()` — discarded. `x.unwrap();`
//! uses the result (to panic on failure). Belief: the result is used.

use std::collections::HashMap;

use tree_sitter::Node;

use super::returns::{Returns, returns};
use super::rules::{self, Rules};
use super::{syntax, z_confidence};
use crate::analyze::bugs::{Detector, Evidence, Finding, Project};
use crate::extraction::detect_language;

pub(super) const RULE: &str = "result-discarded";

/// Sites that must use the result before a discard is a deviation.
const MIN_USED: usize = 3;
/// Share of the sites that must use it.
const MIN_RATIO: f64 = 0.75;
/// Agreeing sites listed as evidence.
const MAX_EVIDENCE: usize = 5;

/// How a call site treats the callee's result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Use {
    Used,
    Discarded(Discard),
    /// Checked for failure, success value dropped (`f()?;`): counts neither
    /// way.
    Checked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Discard {
    /// `f();`
    Statement,
    /// `let _ = f()`, `_ = f()`
    Wildcard,
    /// `match f() { Ok(_) => …, Err(e) => … }`, `if let Err(e) = f()`
    UnboundSuccess,
    /// `f().is_ok()`
    PayloadDropped(&'static str),
    /// `void f()`
    Void,
}

impl Discard {
    fn describe(self) -> String {
        match self {
            Discard::Statement => "called as a statement".to_string(),
            Discard::Wildcard => "bound to `_`".to_string(),
            Discard::UnboundSuccess => "matched without binding the success value".to_string(),
            Discard::PayloadDropped(method) => format!("reduced to `.{method}()`"),
            Discard::Void => "`void`ed".to_string(),
        }
    }
}

/// How the call node `call` (with its `ancestors`, innermost last) treats
/// its result.
pub(super) fn classify<'t>(
    rules: &Rules,
    source: &str,
    call: Node<'t>,
    ancestors: &[Node<'t>],
) -> Use {
    let mut current = call;
    let mut depth = ancestors.len();
    let mut checked = false;
    // A discard after a check (`f()?;`) is a check.
    let discard = |how: Discard, checked: bool| {
        if checked {
            Use::Checked
        } else {
            Use::Discarded(how)
        }
    };
    while depth > 0 {
        depth -= 1;
        let parent = ancestors[depth];
        let kind = parent.kind();
        if rules.transparent.contains(&kind) {
            checked |= rules.checks.contains(&kind);
            current = parent;
            continue;
        }
        if rules.statements.contains(&kind) {
            return if rules.statement_needs_semicolon && !syntax::ends_with_semicolon(parent) {
                Use::Used
            } else {
                discard(Discard::Statement, checked)
            };
        }
        if let Some((_, operator)) = rules.discarding_unary.iter().find(|(k, _)| *k == kind) {
            let voided = parent
                .child_by_field_name("operator")
                .is_some_and(|op| syntax::text(op, source) == *operator);
            return if voided {
                discard(Discard::Void, checked)
            } else {
                Use::Used
            };
        }
        if let Some((_, field)) = rules.lets.iter().find(|(k, _)| *k == kind) {
            // `let _: T = f()` names the type a generic `f` returns: a use.
            if parent.child_by_field_name("type").is_some() {
                return Use::Used;
            }
            return wildcard(parent.child_by_field_name(field), current, source, checked);
        }
        if let Some((_, left)) = rules.assignments.iter().find(|(k, _)| *k == kind) {
            return wildcard(parent.child_by_field_name(left), current, source, checked);
        }
        if let Some((_, field)) = rules.let_conditions.iter().find(|(k, _)| *k == kind) {
            let binds = parent.child_by_field_name(field).is_none_or(|pattern| {
                pattern_binds(rules, syntax::text(pattern, source)) == Bind::Value
            });
            return if binds {
                Use::Used
            } else {
                discard(Discard::UnboundSuccess, checked)
            };
        }
        if rules.matches.contains(&kind)
            && parent.child_by_field_name(rules.match_subject) == Some(current)
        {
            return if match_discards(rules, source, parent) {
                discard(Discard::UnboundSuccess, checked)
            } else {
                Use::Used
            };
        }
        if let Some((_, object, member)) = rules.members.iter().find(|(k, ..)| *k == kind) {
            if parent.child_by_field_name(object) != Some(current) {
                return Use::Used;
            }
            let called = depth > 0
                && rules.calls.contains(&ancestors[depth - 1].kind())
                && ancestors[depth - 1].child_by_field_name("function") == Some(parent);
            if !called {
                return Use::Used;
            }
            let method = parent
                .child_by_field_name(member)
                .map_or("", |name| syntax::text(name, source));
            if let Some(dropping) = rules
                .payload_dropping_methods
                .iter()
                .find(|m| **m == method)
            {
                return discard(Discard::PayloadDropped(dropping), checked);
            }
            if rules.forwarding_methods.contains(&method) {
                depth -= 1;
                current = ancestors[depth];
                continue;
            }
            return Use::Used;
        }
        return Use::Used;
    }
    Use::Used
}

fn wildcard(pattern: Option<Node<'_>>, value: Node<'_>, source: &str, checked: bool) -> Use {
    match pattern {
        Some(pattern) if pattern != value && syntax::text(pattern, source).trim() == "_" => {
            if checked {
                Use::Checked
            } else {
                Use::Discarded(Discard::Wildcard)
            }
        }
        _ => Use::Used,
    }
}

/// What a pattern on a `Result`/`Option` binds.
#[derive(Debug, PartialEq, Eq)]
enum Bind {
    /// `_`
    Nothing,
    /// `Ok(_)`, `Err(e)`, `None`: a variant, no success value bound.
    VariantOnly,
    /// `Ok(v)`, `Some(x)`, anything else.
    Value,
}

fn pattern_binds(rules: &Rules, pattern: &str) -> Bind {
    if rules.success_variants.is_empty() {
        return Bind::Value;
    }
    let compact: String = pattern.chars().filter(|ch| !ch.is_whitespace()).collect();
    let mut result = Bind::Nothing;
    for alternative in compact.split('|') {
        let alternative = alternative.trim_start_matches('&');
        if alternative == "_" {
            continue;
        }
        let (head, inner) = match alternative.split_once('(') {
            Some((head, rest)) => (head, Some(rest.strip_suffix(')').unwrap_or(rest))),
            None => (alternative, None),
        };
        let variant = head.rsplit("::").next().unwrap_or(head);
        // A failure variant, or a success variant binding nothing.
        let unbound = rules.failure_variants.contains(&variant)
            || (rules.success_variants.contains(&variant) && matches!(inner, Some("_" | "..")));
        if !unbound {
            return Bind::Value;
        }
        result = Bind::VariantOnly;
    }
    result
}

/// A `match` on a result none of whose arms binds its success value (and
/// at least one names a variant, so it is a `Result`/`Option` match).
fn match_discards(rules: &Rules, source: &str, node: Node<'_>) -> bool {
    if rules.success_variants.is_empty() {
        return false;
    }
    let Some(body) = node.child_by_field_name("body") else {
        return false;
    };
    let mut variant = false;
    for arm in syntax::children(rules, body) {
        if !rules.arms.contains(&arm.kind()) {
            continue;
        }
        let Some(pattern) = arm.child_by_field_name(rules.arm_pattern) else {
            return false;
        };
        let core = pattern.child(0).unwrap_or(pattern);
        match pattern_binds(rules, syntax::text(core, source)) {
            Bind::Value => return false,
            Bind::VariantOnly => variant = true,
            Bind::Nothing => {}
        }
    }
    variant
}

/// The discarded sites that break a strong belief, with the site each is
/// at (for merging into `arm-result-deviance`).
pub(super) fn findings(project: &Project, uses: &[Option<Use>]) -> Vec<(Finding, usize)> {
    let calls = project.call_sites();
    let mut by_callee: HashMap<&str, (Vec<usize>, Vec<(usize, Discard)>)> = HashMap::new();
    for (site, used) in uses.iter().enumerate() {
        let Some(used) = used else { continue };
        let call = &calls[site];
        if call.in_test || !matches!(call.callee_kind.as_str(), "function" | "method") {
            continue;
        }
        let entry = by_callee.entry(call.callee_id.as_str()).or_default();
        match used {
            Use::Used => entry.0.push(site),
            Use::Discarded(how) => entry.1.push((site, *how)),
            Use::Checked => {}
        }
    }

    let mut out = Vec::new();
    let mut callees: Vec<_> = by_callee.into_iter().collect();
    callees.sort_by(|a, b| a.0.cmp(b.0));
    for (_, (used, discarded)) in callees {
        let total = used.len() + discarded.len();
        if discarded.is_empty()
            || used.len() < MIN_USED
            || (used.len() as f64) < MIN_RATIO * total as f64
        {
            continue;
        }
        let first = &calls[used[0]];
        let Some(rules) = rules::for_language(detect_language(&first.callee_file, None)) else {
            continue;
        };
        let Returns::Value(returned) = returns(rules, first.callee_signature.as_deref()) else {
            continue;
        };
        let mut using: Vec<usize> = used.clone();
        using.sort_by(|&a, &b| {
            (&calls[a].file, calls[a].line).cmp(&(&calls[b].file, calls[b].line))
        });
        let evidence: Vec<Evidence> = using
            .iter()
            .take(MAX_EVIDENCE)
            .map(|&site| Evidence {
                file: calls[site].file.clone(),
                line: calls[site].line,
                note: format!("`{}` uses the result", calls[site].caller),
            })
            .collect();
        let confidence = z_confidence(used.len(), total);
        for (site, how) in discarded {
            let call = &calls[site];
            out.push((
                Finding {
                    detector: Detector::Deviance,
                    rule: RULE,
                    file: call.file.clone(),
                    line: call.line,
                    col: call.col,
                    function: Some(call.caller.clone()),
                    message: format!(
                        "the result of `{}` (`{returned}`) is {} here, while {} of {total} call \
                         sites use it",
                        call.callee_name,
                        how.describe(),
                        used.len(),
                    ),
                    confidence,
                    evidence: evidence.clone(),
                },
                site,
            ));
        }
    }
    out
}

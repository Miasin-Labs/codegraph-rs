//! `result-discarded`: a function whose result most of its call sites use,
//! called where the result is thrown away.
//!
//! Each resolved call is classified from its syntactic context: bound to a
//! name, passed on, returned, tested, chained — used; an expression
//! statement — discarded. `x.unwrap();` uses the result (to panic on
//! failure). Belief: the result is used.
//!
//! Engler's rule is about *unchecked* results. Syntax that names the discard
//! (`let _ = f()`, `_ = f()`, `void f()`), or looks at the outcome and
//! chooses to drop the payload (`if let Err(e) = f()`, `match f() { Ok(_) =>
//! …, Err(e) => … }`, `f().is_ok()`), is a decision the author made, not an
//! oversight: such a site is [`Use::Handled`]. It still weakens the belief
//! (it is not a use), but it is never reported on its own — only as
//! supporting evidence folded into an `arm-result-deviance` finding at the
//! same call, where dropping the value is the arm's bug (the rms `KeepBoth`
//! arm matched `download_file(..)` with `Ok(_)`).
//!
//! A bare statement discard is reported alone only when the result reports
//! success or failure ([`reports_status`]: `Result`-like, `bool`): a
//! returned reference, handle, builder or generic value called for its side
//! effect (`parser.add_child(..);`, `next(src);` to advance a cursor,
//! `with(|x| ..);`) is not an unchecked result.

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
/// …before one is reported on its own: 3 of 4 (z = 1) paired with nothing
/// else was noise on RustSec crates (`array_call`, `ref_dec`, `unsubscribe`
/// each `bool`); 4 of 5 is the least that stands alone.
const MIN_USED_ALONE: usize = 4;
/// Share of the sites that must use it.
const MIN_RATIO: f64 = 0.75;
/// Agreeing sites listed as evidence.
const MAX_EVIDENCE: usize = 5;

/// How a call site treats the callee's result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Use {
    Used,
    /// `f();`: the value is dropped without a word.
    Discarded,
    /// Explicitly dropped or reduced to its outcome: a decision, not an
    /// oversight. Not a use; never reported alone.
    Handled(Explicit),
    /// Checked for failure, success value dropped (`f()?;`): counts neither
    /// way.
    Checked,
}

/// The explicit forms of dropping a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Explicit {
    /// `let _ = f()`, `_ = f()`
    Wildcard,
    /// `match f() { Ok(_) => …, Err(e) => … }`, `if let Err(e) = f()`
    UnboundSuccess,
    /// `f().is_ok()`
    PayloadDropped(&'static str),
    /// `void f()`
    Void,
    /// `f().ok();`: the error turned into an `Option` and dropped — Rust's
    /// spelling of "ignore this failure".
    Silenced(&'static str),
}

impl Use {
    /// The site drops the value (in any form): what `arm-result-deviance`
    /// reads as work whose result the arm did not pass on.
    pub(super) fn drops_value(self) -> bool {
        !matches!(self, Use::Used)
    }

    fn describe(self) -> String {
        match self {
            Use::Discarded => "called as a statement".to_string(),
            Use::Handled(Explicit::Wildcard) => "bound to `_`".to_string(),
            Use::Handled(Explicit::UnboundSuccess) => {
                "matched without binding the success value".to_string()
            }
            Use::Handled(Explicit::PayloadDropped(method)) => {
                format!("reduced to `.{method}()`")
            }
            Use::Handled(Explicit::Void) => "`void`ed".to_string(),
            Use::Handled(Explicit::Silenced(method)) => format!("silenced with `.{method}()`"),
            Use::Used | Use::Checked => String::new(),
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
    // Passed through a method that says the failure is ignored (`.ok()`).
    let mut silenced: Option<&'static str> = None;
    // A discard after a check (`f()?;`) is a check.
    let discard = |how: Use, checked: bool| if checked { Use::Checked } else { how };
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
            if checked_by_try_macro(parent, source) {
                return Use::Checked;
            }
            return if rules.statement_needs_semicolon && !syntax::ends_with_semicolon(parent) {
                Use::Used
            } else if let Some(method) = silenced {
                discard(Use::Handled(Explicit::Silenced(method)), checked)
            } else {
                discard(Use::Discarded, checked)
            };
        }
        if let Some((_, operator)) = rules.discarding_unary.iter().find(|(k, _)| *k == kind) {
            let voided = parent
                .child_by_field_name("operator")
                .is_some_and(|op| syntax::text(op, source) == *operator);
            return if voided {
                discard(Use::Handled(Explicit::Void), checked)
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
                discard(Use::Handled(Explicit::UnboundSuccess), checked)
            };
        }
        if rules.matches.contains(&kind)
            && parent.child_by_field_name(rules.match_subject) == Some(current)
        {
            return if match_discards(rules, source, parent) {
                discard(Use::Handled(Explicit::UnboundSuccess), checked)
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
                return discard(Use::Handled(Explicit::PayloadDropped(dropping)), checked);
            }
            if let Some(silencing) = rules.silencing_methods.iter().find(|m| **m == method) {
                silenced = Some(silencing);
            }
            if silenced.is_some() || rules.forwarding_methods.contains(&method) {
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

/// `try!(f());` — Rust 2015's `?`. `try` is a keyword to the grammar, so
/// the statement parses as an `ERROR` for `try!` and a parenthesized
/// expression: read the text just before the statement (O(1), no sibling
/// walk).
fn checked_by_try_macro(statement: Node<'_>, source: &str) -> bool {
    let start = statement.start_byte();
    source
        .get(start..)
        .is_some_and(|text| text.starts_with('('))
        && source
            .get(..start)
            .is_some_and(|before| before.trim_end().ends_with("try!"))
}

fn wildcard(pattern: Option<Node<'_>>, value: Node<'_>, source: &str, checked: bool) -> Use {
    match pattern {
        Some(pattern) if pattern != value && syntax::text(pattern, source).trim() == "_" => {
            if checked {
                Use::Checked
            } else {
                Use::Handled(Explicit::Wildcard)
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

/// A site that departs from a strong belief, and whether it may be
/// reported on its own (a bare statement discard of a status result) or
/// only folded into an arm finding at the same call.
pub(super) struct Departure {
    pub finding: Finding,
    pub site: usize,
    pub standalone: bool,
}

/// The sites that drop a result most call sites use, with the site each is
/// at (for merging into `arm-result-deviance`).
pub(super) fn findings(project: &Project, uses: &[Option<Use>]) -> Vec<Departure> {
    let calls = project.call_sites();
    let mut by_callee: HashMap<&str, (Vec<usize>, Vec<(usize, Use)>)> = HashMap::new();
    for (site, used) in uses.iter().enumerate() {
        let Some(used) = used else { continue };
        let call = &calls[site];
        if call.in_test || !matches!(call.callee_kind.as_str(), "function" | "method") {
            continue;
        }
        let entry = by_callee.entry(call.callee_id.as_str()).or_default();
        match used {
            Use::Used => entry.0.push(site),
            Use::Discarded | Use::Handled(_) => entry.1.push((site, *used)),
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
        let status = reports_status(rules, &returned);
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
            out.push(Departure {
                finding: Finding {
                    detector: Detector::Deviance,
                    rule: RULE.into(),
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
                standalone: how == Use::Discarded && status && used.len() >= MIN_USED_ALONE,
            });
        }
    }
    out
}

/// Whether a declared result reports success or failure — what an
/// unchecked-result rule is about: a `Result`-like type (`Result<T>`,
/// `io::Result<T>`, `LockResult<G>`) or a boolean, through references and
/// `Promise<…>`. A reference, handle, builder, `Option` (a maybe-value, not
/// an error) or type parameter (`R` from `with(|x| ..)`) is not.
pub(super) fn reports_status(rules: &Rules, declared: &str) -> bool {
    let mut ty = declared.trim();
    loop {
        let before = ty;
        ty = ty.trim_start_matches('&').trim_start();
        if let Some(rest) = ty.strip_prefix('\'') {
            // A lifetime: `&'a T`.
            ty = rest
                .trim_start_matches(|c: char| c.is_alphanumeric() || c == '_')
                .trim_start();
        }
        ty = ty.strip_prefix("mut ").unwrap_or(ty).trim_start();
        for wrapper in rules.status_transparent {
            if let Some(inner) = ty
                .strip_prefix(wrapper)
                .and_then(|rest| rest.strip_prefix('<'))
                .and_then(|rest| rest.strip_suffix('>'))
            {
                ty = inner.trim();
            }
        }
        if ty == before {
            break;
        }
    }
    let head = ty.split('<').next().unwrap_or(ty).trim();
    let name = head.rsplit([':', '.']).next().unwrap_or(head);
    rules.status_types.contains(&name)
        || rules
            .status_suffixes
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

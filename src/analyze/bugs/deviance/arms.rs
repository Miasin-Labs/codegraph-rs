//! `arm-result-deviance`: in a `match`/`switch` whose value is the
//! function's result, the arms that did work mostly return what a project
//! call produced (`UseLocal => self.upload(..)`). An arm that makes a
//! project call and then yields a constant (`KeepBoth => { download(..);
//! None }`) dropped what its work produced — the caller cannot tell it
//! happened.
//!
//! Two shapes that look alike are not deviant. An arm yielding only success
//! (`Ok(())`, `()`) reports all a sibling could when the work's failures
//! went up through `?` (a `Result<()>` lowering, `Val::store`), so it is
//! neutral. And the work must be *dropped*: an arm whose project calls all
//! hand their value on (to a log line, a binding it reads) did not lose it —
//! [`retain_dropped`] keeps only arms with a call whose result is discarded,
//! explicitly dropped (`Ok(_) =>`) or only checked (`f()?;`).

use tree_sitter::Node;

use super::results::{Departure, Use};
use super::returns::{Returns, returns};
use super::rules::{self, ArmValue, Rules};
use super::syntax::{self, Value};
use super::{FileScan, sites_between, snippet};
use crate::analyze::bugs::{CallSite, Detector, Evidence, Finding};
use crate::ensure_sufficient_stack;
use crate::extraction::detect_language;

pub(super) const RULE: &str = "arm-result-deviance";

/// Arms a match needs before its arms make a belief.
const MIN_ARMS: usize = 3;
/// Sibling arms that must yield a project call's result.
const MIN_AGREEING: usize = 2;

/// A deviant arm, with what `result-discarded` may merge into it.
pub(super) struct ArmDeviance {
    pub finding: Finding,
    /// Lines of the arm's body.
    pub lines: (u32, u32),
    /// The project call sites the arm makes.
    pub sites: Vec<usize>,
    /// The message after the calls it names (`but yields …`): completed by
    /// [`retain_dropped`] once the dropped calls are known.
    label: String,
    tail: String,
}

/// The `match` nodes whose value is the value of `node` (a function body,
/// a `return`): the tail, through blocks, `if`/`else`, wrappers
/// (`Ok(match …)`) and the arms of a match that is itself the result.
pub(super) fn result_matches<'t>(
    rules: &Rules,
    source: &str,
    node: Node<'t>,
    out: &mut Vec<Node<'t>>,
) {
    ensure_sufficient_stack(|| {
        let node = syntax::strip(rules, node);
        let kind = node.kind();
        if rules.matches.contains(&kind) {
            out.push(node);
            for arm in arms_of(rules, node) {
                if let Some((body, _)) = arm_body(rules, arm) {
                    result_matches(rules, source, body, out);
                }
            }
        } else if rules.blocks.contains(&kind) || rules.returns.contains(&kind) {
            if let Value::Expr(value) = syntax::resolve(rules, node) {
                if value != node {
                    result_matches(rules, source, value, out);
                }
            }
        } else if rules.conditionals.contains(&kind) {
            for field in ["consequence", "alternative"] {
                let Some(mut branch) = node.child_by_field_name(field) else {
                    continue;
                };
                if rules.else_clauses.contains(&branch.kind()) {
                    match syntax::first_child(rules, branch) {
                        Some(inner) => branch = inner,
                        None => continue,
                    }
                }
                result_matches(rules, source, branch, out);
            }
        } else if let Some(inner) = syntax::wrapped(rules, node, source) {
            result_matches(rules, source, inner, out);
        }
    });
}

fn arms_of<'t>(rules: &Rules, node: Node<'t>) -> Vec<Node<'t>> {
    node.child_by_field_name("body")
        .map(|body| {
            syntax::children(rules, body)
                .into_iter()
                .filter(|arm| rules.arms.contains(&arm.kind()))
                .collect()
        })
        .unwrap_or_default()
}

/// The node holding an arm's body, and a child of it that is the arm's
/// label rather than a statement (a `case`'s value).
fn arm_body<'t>(rules: &Rules, arm: Node<'t>) -> Option<(Node<'t>, Option<Node<'t>>)> {
    if rules.arm_body.is_empty() {
        Some((arm, arm.child_by_field_name("value")))
    } else {
        arm.child_by_field_name(rules.arm_body)
            .map(|body| (body, None))
    }
}

/// The arm's pattern (or `case` label) node.
fn arm_pattern<'t>(rules: &Rules, arm: Node<'t>) -> Option<Node<'t>> {
    if !rules.arm_pattern.is_empty() {
        return arm.child_by_field_name(rules.arm_pattern);
    }
    arm.child_by_field_name("value").or_else(|| {
        syntax::first_child(rules, arm).filter(|first| {
            !rules.blocks.contains(&first.kind())
                && Some(*first) != arm_body(rules, arm).map(|b| b.0)
        })
    })
}

/// The arm as it reads up to its body: `ConflictResolution::KeepBoth {
/// renamed_to }`, `case 'x'`, `default`.
fn arm_label(rules: &Rules, arm: Node<'_>, source: &str) -> String {
    if rules.default_arms.contains(&arm.kind()) {
        return "default".to_string();
    }
    let pattern = arm_pattern(rules, arm)
        .map(|pattern| syntax::text(pattern, source))
        .unwrap_or("");
    let label = if rules.arm_pattern.is_empty() {
        format!("case {pattern}")
    } else {
        pattern.to_string()
    };
    snippet(&label.split_whitespace().collect::<Vec<_>>().join(" "), 70)
}

/// A wildcard or catch-all arm (`_ =>`, `other =>`, `default:`, `case _:`):
/// it handles "everything else", so no belief binds it.
fn is_default(rules: &Rules, arm: Node<'_>, source: &str) -> bool {
    if rules.default_arms.contains(&arm.kind()) {
        return true;
    }
    let Some(pattern) = arm_pattern(rules, arm) else {
        return false;
    };
    let guarded = pattern.child_by_field_name("condition").is_some()
        || arm.child_by_field_name("guard").is_some();
    if guarded {
        return false;
    }
    // Rust's `match_pattern` wraps the pattern (and guard): read its first
    // child.
    let core = if rules.arm_pattern.is_empty() {
        pattern
    } else {
        pattern.child(0).unwrap_or(pattern)
    };
    let core = syntax::text(core, source).trim();
    core == "_"
        || (rules.bare_identifier_binds
            && core.starts_with(|ch: char| ch.is_ascii_lowercase() || ch == '_')
            && core.chars().all(|ch| ch == '_' || ch.is_alphanumeric()))
}

/// What an arm yields.
enum Yield {
    /// A project call's result (the call site).
    Work(usize),
    /// A constant (its text).
    Constant(String),
    Other,
}

fn classify_value(scan: &FileScan<'_>, node: Node<'_>) -> Yield {
    ensure_sufficient_stack(|| {
        let rules = scan.rules;
        let node = syntax::strip(rules, node);
        if syntax::is_constant(rules, node, scan.source) {
            return Yield::Constant(snippet(syntax::text(node, scan.source), 40));
        }
        if let Some(inner) = syntax::wrapped(rules, node, scan.source) {
            return match syntax::resolve(rules, inner) {
                Value::Expr(inner) => classify_value(scan, inner),
                _ => Yield::Other,
            };
        }
        if let Some(site) = scan.project_call(node) {
            return Yield::Work(site);
        }
        // A method chained on a project call's result (`self.load(x)?.len()`)
        // still reports it.
        if rules.calls.contains(&node.kind()) {
            if let Some(receiver) = syntax::receiver(rules, node) {
                if let Yield::Work(site) = classify_value(scan, receiver) {
                    return Yield::Work(site);
                }
            }
        }
        Yield::Other
    })
}

/// The call's callee returns something (or may: unknown counts).
fn returns_something(scan: &FileScan<'_>, site: usize) -> bool {
    let call = &scan.project.call_sites()[site];
    if !matches!(call.callee_kind.as_str(), "function" | "method") {
        return false;
    }
    rules::for_language(detect_language(&call.callee_file, None))
        .is_none_or(|rules| returns(rules, call.callee_signature.as_deref()) != Returns::Unit)
}

/// `body` holds a `return` of a non-constant value (outside nested
/// functions and closures). Walked only for constant-valued arms that make
/// project calls, with an explicit stack.
fn returns_early(scan: &FileScan<'_>, body: Node<'_>) -> bool {
    let rules = scan.rules;
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        let kind = node.kind();
        if node != body && rules.functions.contains(&kind) {
            continue;
        }
        if rules.returns.contains(&kind) {
            let constant = syntax::first_child(rules, node)
                .is_none_or(|value| syntax::is_constant(rules, value, scan.source));
            if !constant {
                return true;
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    false
}

struct ArmInfo<'t> {
    arm: Node<'t>,
    label: String,
    kind: ArmKind,
}

enum ArmKind {
    Work(usize),
    /// Yields `constant` after the project calls at these sites.
    Deviant {
        constant: String,
        sites: Vec<usize>,
        body: (u32, u32),
    },
    Neutral,
}

/// Check one match: the deviant arms, if the others make a strong belief.
pub(super) fn check(scan: &FileScan<'_>, node: Node<'_>) -> Option<Vec<ArmDeviance>> {
    let rules = scan.rules;
    let arms = arms_of(rules, node);
    if arms.len() < MIN_ARMS {
        return None;
    }
    let mut infos = Vec::with_capacity(arms.len());
    for arm in arms {
        let label = arm_label(rules, arm, scan.source);
        if is_default(rules, arm, scan.source) {
            infos.push(ArmInfo {
                arm,
                label,
                kind: ArmKind::Neutral,
            });
            continue;
        }
        let Some((body, skip)) = arm_body(rules, arm) else {
            continue;
        };
        let value = match rules.arm_value {
            ArmValue::Expression => syntax::resolve(rules, body),
            ArmValue::Returned => syntax::final_value(rules, body, skip),
        };
        let success_only = matches!(
            value,
            Value::Expr(v) if syntax::is_unit_value(rules, v, scan.source)
        );
        let yielded = match value {
            Value::Expr(value) => classify_value(scan, value),
            Value::Unit => Yield::Constant("()".to_string()),
            Value::Absent => Yield::Other,
        };
        let kind = match yielded {
            // A call returning nothing reports nothing (`() => lower_if(..)`).
            Yield::Work(site) if returns_something(scan, site) => ArmKind::Work(site),
            Yield::Work(_) => ArmKind::Neutral,
            // Success and nothing else: all a `Result<()>` arm can report.
            Yield::Constant(_) if success_only => ArmKind::Neutral,
            Yield::Constant(constant) => {
                let from = skip.map_or(syntax::start(body), syntax::end);
                let sites: Vec<usize> = sites_between(scan.sites, from, syntax::end(body))
                    .iter()
                    .map(|pos| pos.site)
                    .filter(|&site| returns_something(scan, site))
                    .collect();
                // An arm that returns its work early (`for … { return
                // Some(x) } None`) reports it.
                if sites.is_empty() || returns_early(scan, body) {
                    ArmKind::Neutral
                } else {
                    ArmKind::Deviant {
                        constant,
                        sites,
                        body: (syntax::start(body).0, syntax::end(body).0),
                    }
                }
            }
            Yield::Other => ArmKind::Neutral,
        };
        infos.push(ArmInfo { arm, label, kind });
    }

    let agreeing: Vec<&ArmInfo<'_>> = infos
        .iter()
        .filter(|info| matches!(info.kind, ArmKind::Work(_)))
        .collect();
    let deviants = infos
        .iter()
        .filter(|info| matches!(info.kind, ArmKind::Deviant { .. }))
        .count();
    // A belief needs 2+ agreeing arms and at least 3 in 5 of the arms that
    // did work.
    if agreeing.len() < MIN_AGREEING
        || deviants == 0
        || agreeing.len() * 5 < (agreeing.len() + deviants) * 3
    {
        return None;
    }
    let ratio = agreeing.len() as f64 / (agreeing.len() + deviants) as f64;
    let calls = scan.project.call_sites();
    let callee = |site: usize| calls[site].callee_name.as_str();
    let mut agreeing_names: Vec<&str> = agreeing
        .iter()
        .filter_map(|info| match info.kind {
            ArmKind::Work(site) => Some(callee(site)),
            _ => None,
        })
        .collect();
    agreeing_names.dedup();
    let evidence: Vec<Evidence> = agreeing
        .iter()
        .filter_map(|info| match info.kind {
            ArmKind::Work(site) => Some(Evidence {
                file: scan.file.to_string(),
                line: syntax::start(info.arm).0,
                note: format!("`{}` yields `{}(..)`", info.label, callee(site)),
            }),
            _ => None,
        })
        .collect();
    let with_values = infos
        .iter()
        .filter(|info| !matches!(info.kind, ArmKind::Neutral))
        .count();

    let deviances = infos
        .iter()
        .filter_map(|info| {
            let ArmKind::Deviant {
                constant,
                sites,
                body,
            } = &info.kind
            else {
                return None;
            };
            let (line, col) = syntax::start(info.arm);
            let tail = format!(
                "but yields `{constant}`, while {} of {with_values} arms that do work yield a \
                 project call's result ({}): what this arm did is not reported to the caller",
                agreeing.len(),
                agreeing_names
                    .iter()
                    .map(|name| format!("`{name}`"))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            Some(ArmDeviance {
                finding: Finding {
                    detector: Detector::Deviance,
                    rule: RULE.into(),
                    file: scan.file.to_string(),
                    line,
                    col,
                    function: scan
                        .project
                        .enclosing_function(scan.file, line)
                        .map(|span| span.qualified_name.clone()),
                    message: String::new(),
                    confidence: 0.4 + 0.5 * ratio,
                    evidence: evidence.clone(),
                },
                lines: *body,
                sites: sites.clone(),
                label: info.label.clone(),
                tail,
            })
        })
        .collect();
    Some(deviances)
}

/// Keep the deviant arms that drop a project call's result — discarded,
/// explicitly dropped or only checked — narrowed to those calls. An arm
/// whose calls all pass their value on (`warn!("{}", c.algorithm())`,
/// `let (p, n) = lower_list(..)?` read later) did not lose its work.
pub(super) fn retain_dropped(
    arms: &mut Vec<ArmDeviance>,
    uses: &[Option<Use>],
    calls: &[CallSite],
) {
    arms.retain_mut(|arm| {
        arm.sites
            .retain(|&site| uses[site].is_some_and(Use::drops_value));
        if arm.sites.is_empty() {
            return false;
        }
        let mut made: Vec<&str> = arm
            .sites
            .iter()
            .map(|&site| calls[site].callee_name.as_str())
            .collect();
        made.dedup();
        let made = made
            .iter()
            .take(3)
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join(", ");
        arm.finding.message = format!("arm `{}` calls {made} {}", arm.label, arm.tail);
        true
    });
}

/// The arm findings, with each `result-discarded` departure at a call the
/// deviant arm makes folded into it (one lead, one root cause: the arm drops
/// the value; the discarded call is how). Merged findings gain confidence
/// (noisy-or of the two); unmerged departures pass through only when they
/// may stand alone (see [`Departure`]).
pub(super) fn merge(mut arms: Vec<ArmDeviance>, departures: Vec<Departure>) -> Vec<Finding> {
    let mut out = Vec::new();
    for Departure {
        finding,
        site,
        standalone,
    } in departures
    {
        let host = arms.iter_mut().find(|arm| {
            arm.finding.file == finding.file
                && arm.lines.0 <= finding.line
                && finding.line <= arm.lines.1
                && arm.sites.contains(&site)
        });
        match host {
            Some(arm) => {
                let a = arm.finding.confidence;
                let b = finding.confidence;
                arm.finding.confidence = (1.0 - (1.0 - a) * (1.0 - b)).min(0.97);
                arm.finding.evidence.push(Evidence {
                    file: finding.file.clone(),
                    line: finding.line,
                    note: format!("result-discarded: {}", finding.message),
                });
                arm.finding.evidence.extend(finding.evidence);
            }
            None if standalone => out.push(finding),
            None => {}
        }
    }
    out.extend(arms.into_iter().map(|arm| arm.finding));
    out
}

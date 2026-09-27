//! `self-comparison` and `constant-condition`.
//!
//! A comparison, logical or arithmetic operation whose two operands are the
//! same side-effect-free expression (`x == x`, `a.b < a.b`, `p && p`,
//! `x - x`) always yields the same value; one operand was meant to be
//! something else. `x != x` is the NaN test and is left alone.
//!
//! An `if`/ternary condition made only of literals (`1 == 1`), or decided by
//! a literal operand (`false && x`, `x || true`), never changes. A bare
//! `if true`/`if false`/`if 0` is a deliberate switch and is left alone, as
//! are loops (`while true` is the infinite-loop idiom, `do … while (0)` a
//! block idiom) and anything naming a constant or macro (`if DEBUG`,
//! `cfg!(…)`).

use tree_sitter::Node;

use super::rules::Pick;
use super::syntax::{is_pure, named_children, path_roots, pick, same_tokens, text, unparen, walk};
use super::{CONSTANT_CONDITION, Ctx, SELF_COMPARISON};

/// Operators whose result is fixed when both operands are equal.
/// Bitwise `&`/`|` are left out: `hash & hash` is JavaScript's idiom for
/// "to 32-bit integer".
const SELF_OPERATORS: &[&str] = &[
    "==", "===", "<", ">", "<=", ">=", "&&", "||", "and", "or", "-", "/", "%", "^", "is", "&^",
];

pub(super) fn check_self_comparison(node: Node<'_>, ctx: &mut Ctx<'_>) {
    let rules = ctx.rules;
    let source = ctx.source;
    let Some(shape) = rules.binaries.iter().find(|b| b.kind == node.kind()) else {
        return;
    };
    if matches!(shape.right, Pick::Nth(_)) && named_children(node, rules).len() != 2 {
        // `a < b < c`: a chain, not one comparison.
        return;
    }
    let Some(operator) = node.child_by_field_name(shape.operator) else {
        return;
    };
    let operator = text(operator, source);
    if !SELF_OPERATORS.contains(&operator) {
        return;
    }
    let (Some(left), Some(right)) = (
        pick(node, shape.left, rules),
        pick(node, shape.right, rules),
    ) else {
        return;
    };
    if left.id() == right.id()
        || !is_pure(left, rules, source)
        || path_roots(left, rules, source).is_empty()
        || !same_tokens(unparen(left, rules), unparen(right, rules), rules, source)
    {
        return;
    }
    if matches!(operator, "==" | "===") && rules.idents.contains(&unparen(left, rules).kind()) {
        // `x === x` on a plain variable is the other NaN test.
        return;
    }
    let message = format!(
        "`{}` compares `{}` with itself: the result never depends on it",
        ctx.snippet(node),
        ctx.snippet(left)
    );
    ctx.report(
        "self-comparison",
        node,
        SELF_COMPARISON,
        message,
        Vec::new(),
    );
}

pub(super) fn check_constant_condition(node: Node<'_>, ctx: &mut Ctx<'_>) {
    let rules = ctx.rules;
    let kind = node.kind();
    let condition = if let Some(shape) = rules.ifs.iter().find(|s| s.kind == kind) {
        node.child_by_field_name(shape.condition)
    } else if let Some(&(_, field)) = rules.conditions.iter().find(|(k, _)| *k == kind) {
        node.child_by_field_name(field)
    } else if let Some(shape) = rules.ternaries.iter().find(|t| t.kind == kind) {
        pick(node, shape.condition, rules)
    } else {
        None
    };
    let Some(condition) = condition else {
        return;
    };
    let condition = unparen(condition, rules);
    let Some(verdict) = constant_value(condition, ctx) else {
        return;
    };
    let message = format!("condition `{}` is always {verdict}", ctx.snippet(condition));
    ctx.report(
        "constant-condition",
        condition,
        CONSTANT_CONDITION,
        message,
        Vec::new(),
    );
}

/// `Some("true"/"false"/"constant")` when the condition cannot change.
fn constant_value(condition: Node<'_>, ctx: &Ctx<'_>) -> Option<&'static str> {
    let rules = ctx.rules;
    let source = ctx.source;
    // Decided by a literal operand of a short-circuit operator.
    if let Some(shape) = rules.binaries.iter().find(|b| b.kind == condition.kind()) {
        let operator = condition
            .child_by_field_name(shape.operator)
            .map(|o| text(o, source))
            .unwrap_or("");
        let operands = [
            pick(condition, shape.left, rules),
            pick(condition, shape.right, rules),
        ];
        let is_literal = |values: &[&str]| {
            operands
                .iter()
                .flatten()
                .any(|operand| values.contains(&text(unparen(*operand, rules), source)))
        };
        match operator {
            "&&" | "and" if is_literal(rules.bool_false) => return Some("false"),
            "||" | "or" if is_literal(rules.bool_true) => return Some("true"),
            _ => {}
        }
    }
    // Literals and operators only, and more than a bare literal.
    if condition.named_child_count() == 0
        || rules.literals.contains(&condition.kind())
        || !is_pure(condition, rules, source)
    {
        return None;
    }
    let mut names_something = false;
    let mut literals = 0;
    walk(condition, |n| {
        let kind = n.kind();
        if rules.idents.contains(&kind) || rules.constant_paths.contains(&kind) {
            names_something = true;
        }
        if rules.literals.contains(&kind) {
            literals += 1;
            return false;
        }
        !names_something
    });
    if names_something || literals < 2 {
        // `!true`, `-1`: a bare literal in disguise, as deliberate as one.
        return None;
    }
    Some("the same (it names no variable)")
}

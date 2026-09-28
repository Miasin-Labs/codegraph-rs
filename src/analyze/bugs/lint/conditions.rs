//! `self-comparison`, `constant-condition`, `comparison-discarded` and
//! `assignment-in-condition`.
//!
//! A comparison, logical or arithmetic operation whose two operands are the
//! same side-effect-free expression (`x == x`, `a.b < a.b`, `p && p`,
//! `x - x`, Java's `s.equals(s)`) always yields the same value; one operand
//! was meant to be something else. `x != x` is the NaN test and is left
//! alone.
//!
//! An `if`/ternary condition made only of literals (`1 == 1`), or decided by
//! a literal operand (`false && x`, `x || true`), never changes. A bare
//! `if true`/`if false`/`if 0` is a deliberate switch and is left alone, as
//! are loops (`while true` is the infinite-loop idiom, `do … while (0)` a
//! block idiom) and anything naming a constant or macro (`if DEBUG`,
//! `cfg!(…)`). A variable compared past its own type's bound (`x <
//! Integer.MIN_VALUE`, `x > INT_MAX` for an `int x` declared in the
//! function) never holds either; a wider variable (`long x > INT_MAX`) is the
//! overflow check and is left alone.
//!
//! A comparison whose value is dropped (`a == b;`, CWE-482) was meant to be
//! an assignment. An assignment standing as an `if`/`while` condition (`if
//! (x = 5)`, CWE-481) was meant to be a comparison — reported when the value
//! assigned is side-effect free (a literal, a variable), since `if ((p =
//! next()))` and `while ((line = read()))` are idioms; where the language's
//! tools accept doubled parentheses as "I mean it" (C, JavaScript), those
//! are honoured too, except around a literal (the condition is then fixed).

use tree_sitter::Node;

use super::rules::{Pick, Rules};
use super::syntax::{
    enclosing_function,
    is_pure,
    named_children,
    path_roots,
    pick,
    same_tokens,
    text,
    unparen,
    walk,
};
use super::{
    ASSIGNMENT_IN_CONDITION,
    COMPARISON_DISCARDED,
    CONSTANT_CONDITION,
    Ctx,
    LIMIT_CONDITION,
    SELF_COMPARISON,
};

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

/// Comparison operators: a comparison's only effect is its value.
const COMPARISONS: &[&str] = &[
    "==", "!=", "===", "!==", "<", ">", "<=", ">=", "is", "is not", "in", "not in",
];

/// The condition an `if`, `elif` or ternary tests.
fn branch_condition<'t>(node: Node<'t>, rules: &Rules) -> Option<Node<'t>> {
    let kind = node.kind();
    if let Some(shape) = rules.ifs.iter().find(|s| s.kind == kind) {
        node.child_by_field_name(shape.condition)
    } else if let Some(&(_, field)) = rules.conditions.iter().find(|(k, _)| *k == kind) {
        node.child_by_field_name(field)
    } else if let Some(shape) = rules.ternaries.iter().find(|t| t.kind == kind) {
        pick(node, shape.condition, rules)
    } else {
        None
    }
}

/// `s.equals(s)`, `a.b.compareTo(a.b)` over a side-effect-free receiver.
pub(super) fn check_self_equals(node: Node<'_>, ctx: &mut Ctx<'_>) {
    let rules = ctx.rules;
    let source = ctx.source;
    let Some(shape) = rules.eq_methods.iter().find(|m| m.kind == node.kind()) else {
        return;
    };
    let name = node
        .child_by_field_name(shape.name)
        .map_or("", |name| text(name, source));
    if !shape.names.contains(&name) {
        return;
    }
    let (Some(object), Some(arguments)) = (
        node.child_by_field_name(shape.object),
        node.child_by_field_name(shape.arguments),
    ) else {
        return;
    };
    let [argument] = named_children(arguments, rules)[..] else {
        return;
    };
    let object = unparen(object, rules);
    if !is_pure(object, rules, source)
        || path_roots(object, rules, source).is_empty()
        || !same_tokens(object, unparen(argument, rules), rules, source)
    {
        return;
    }
    let message = format!(
        "`{}` compares `{}` with itself: the result never depends on it",
        ctx.snippet(node),
        ctx.snippet(object)
    );
    ctx.report(
        "self-comparison",
        node,
        SELF_COMPARISON,
        message,
        Vec::new(),
    );
}

/// `a == b;`: a comparison whose value nothing reads.
pub(super) fn check_comparison_discarded(node: Node<'_>, ctx: &mut Ctx<'_>) {
    let rules = ctx.rules;
    let source = ctx.source;
    let [expression] = named_children(node, rules)[..] else {
        return;
    };
    let expression = unparen(expression, rules);
    let Some(shape) = rules.binaries.iter().find(|b| b.kind == expression.kind()) else {
        return;
    };
    let operator = operator_text(expression, shape.operator, source);
    if !COMPARISONS.contains(&operator.as_str()) {
        return;
    }
    let message = format!(
        "the comparison `{}` is a statement: its result is dropped (an assignment was meant?)",
        ctx.snippet(expression)
    );
    ctx.report(
        "comparison-discarded",
        expression,
        COMPARISON_DISCARDED,
        message,
        Vec::new(),
    );
}

/// An operator by its field, whitespace collapsed (`not in`, `is not`).
fn operator_text(node: Node<'_>, field: &str, source: &str) -> String {
    let mut cursor = node.walk();
    let parts: Vec<&str> = node
        .children_by_field_name(field, &mut cursor)
        .map(|op| text(op, source))
        .collect();
    parts
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// `if (x = 5)`, `while (flag = other)`: an assignment tested as a
/// condition.
pub(super) fn check_assignment_condition(node: Node<'_>, ctx: &mut Ctx<'_>) {
    let rules = ctx.rules;
    let source = ctx.source;
    if rules.cond_assignments.is_empty() {
        return;
    }
    let condition = if let Some(shape) = rules.cond_loops.iter().find(|l| l.kind == node.kind()) {
        pick(node, shape.condition, rules)
    } else if rules.ifs.iter().any(|s| s.kind == node.kind()) {
        branch_condition(node, rules)
    } else {
        None
    };
    let Some(condition) = condition else {
        return;
    };
    // Count the parentheses around the assignment (a C++ `condition_clause`
    // is the statement's own pair).
    let mut inner = condition;
    let mut parens = 0;
    while rules.parens.contains(&inner.kind()) {
        match named_children(inner, rules)[..] {
            [only] => {
                parens += 1;
                inner = only;
            }
            _ => break,
        }
    }
    if !rules.cond_assignments.contains(&inner.kind()) {
        return;
    }
    let (Some(left), Some(right)) = (
        inner.child_by_field_name("left"),
        inner.child_by_field_name("right"),
    ) else {
        return;
    };
    // `=` only: `+=` and friends are not a slip for `==`.
    let operator = source
        .get(left.end_byte()..right.start_byte())
        .unwrap_or("")
        .trim();
    if operator != "=" {
        return;
    }
    let value = unparen(right, rules);
    if !is_pure(value, rules, source) {
        return;
    }
    let literal = rules.literals.contains(&value.kind())
        || (value.named_child_count() == 0 && path_roots(value, rules, source).is_empty());
    if rules.double_parens_mean_it && parens >= 2 && !literal {
        return;
    }
    if assigns_for_the_branch(node, left, value, rules, source) {
        return;
    }
    let message = format!(
        "the condition assigns (`{}`) rather than compares: `==` was meant?",
        ctx.snippet(inner)
    );
    ctx.report(
        "assignment-in-condition",
        inner,
        ASSIGNMENT_IN_CONDITION,
        message,
        Vec::new(),
    );
}

/// `if (result = item != null && item[RAW]) return result;`: a computed
/// value (an operation, not a literal or a variable) assigned to the
/// variable the branch then reads — assign-and-test, the idiom minifiers
/// and hand-written JS use alike (a bundled library's `unwrap`, 2026-09).
/// A slip for `==` leaves the variable unread (`if (x = y) { g(); }`).
fn assigns_for_the_branch(
    statement: Node<'_>,
    left: Node<'_>,
    value: Node<'_>,
    rules: &Rules,
    source: &str,
) -> bool {
    let computed = rules.binaries.iter().any(|b| b.kind == value.kind())
        || rules.ternaries.iter().any(|t| t.kind == value.kind());
    if !computed || !rules.idents.contains(&left.kind()) {
        return false;
    }
    let name = text(left, source);
    let Some(branch) = statement
        .child_by_field_name("consequence")
        .or_else(|| statement.child_by_field_name("body"))
    else {
        return false;
    };
    let mut read = false;
    walk(branch, |n| {
        if !read && rules.idents.contains(&n.kind()) && text(n, source) == name {
            read = true;
        }
        !read
    });
    read
}

/// `x < Integer.MIN_VALUE`, `INT_MAX < x`, `x >= INT_MIN` for a variable
/// declared (in the enclosing function) with a type the bound covers.
pub(super) fn check_limit_comparison(node: Node<'_>, ctx: &mut Ctx<'_>) {
    let rules = ctx.rules;
    let source = ctx.source;
    if rules.limits.is_empty() {
        return;
    }
    let Some(shape) = rules.binaries.iter().find(|b| b.kind == node.kind()) else {
        return;
    };
    let operator = operator_text(node, shape.operator, source);
    let (Some(left), Some(right)) = (
        pick(node, shape.left, rules),
        pick(node, shape.right, rules),
    ) else {
        return;
    };
    let (left, right) = (unparen(left, rules), unparen(right, rules));
    let compact = |n: Node<'_>| -> String {
        text(n, source)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect()
    };
    // Normalise to `variable <op> bound`.
    let (variable, limit, operator) =
        if let Some(limit) = rules.limits.iter().find(|l| l.name == compact(right)) {
            (left, limit, operator.as_str())
        } else if let Some(limit) = rules.limits.iter().find(|l| l.name == compact(left)) {
            let flipped = match operator.as_str() {
                "<" => ">",
                ">" => "<",
                "<=" => ">=",
                ">=" => "<=",
                _ => return,
            };
            (right, limit, flipped)
        } else {
            return;
        };
    let verdict = match (limit.is_max, operator) {
        (false, "<") | (true, ">") => "false",
        (false, ">=") | (true, "<=") => "true",
        _ => return,
    };
    if !rules.idents.contains(&variable.kind()) {
        return;
    }
    let name = text(variable, source);
    let Some(function) = enclosing_function(node, rules) else {
        return;
    };
    let types = declared_types(function, name, rules, source);
    if types.is_empty() || !types.iter().all(|ty| limit.types.contains(&ty.as_str())) {
        return;
    }
    let message = format!(
        "condition `{}` is always {verdict}: `{name}` is a `{}`, which never goes past `{}`",
        ctx.snippet(node),
        types[0],
        limit.name
    );
    ctx.report(
        "constant-condition",
        node,
        LIMIT_CONDITION,
        message,
        Vec::new(),
    );
}

/// The declared types (whitespace-normalised) of every declaration of
/// `name` in `function`; empty when none is found.
fn declared_types(function: Node<'_>, name: &str, rules: &Rules, source: &str) -> Vec<String> {
    let mut out = Vec::new();
    walk(function, |n| {
        let Some(&(_, type_field, declarator_field)) = rules
            .typed_declarations
            .iter()
            .find(|(k, ..)| *k == n.kind())
        else {
            return true;
        };
        let Some(ty) = n.child_by_field_name(type_field) else {
            return true;
        };
        let mut cursor = n.walk();
        for declarator in n.children_by_field_name(declarator_field, &mut cursor) {
            if declared_name(declarator, rules, source) == Some(name) {
                out.push(
                    text(ty, source)
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" "),
                );
            }
        }
        true
    });
    out
}

/// The variable a declarator declares: the name itself, or under a
/// `name`/`declarator` field of a plain wrapper (`x = 1`). A pointer or
/// array declarator declares another type.
fn declared_name<'s>(declarator: Node<'_>, rules: &Rules, source: &'s str) -> Option<&'s str> {
    if rules.idents.contains(&declarator.kind()) {
        return Some(text(declarator, source));
    }
    if !matches!(declarator.kind(), "variable_declarator" | "init_declarator") {
        return None;
    }
    let inner = declarator
        .child_by_field_name("name")
        .or_else(|| declarator.child_by_field_name("declarator"))?;
    rules
        .idents
        .contains(&inner.kind())
        .then(|| text(inner, source))
}

pub(super) fn check_constant_condition(node: Node<'_>, ctx: &mut Ctx<'_>) {
    let rules = ctx.rules;
    let Some(condition) = branch_condition(node, rules) else {
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
            // A build-time substitution (`"@INSTALL_TYPE@" == "MODULE"`,
            // filled in by configure/meson): constant only unbuilt.
            names_something |= is_substitution_placeholder(text(n, source));
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

/// A string literal that is a configure/meson/CMake substitution: `@NAME@`
/// (quotes, if any, around it).
fn is_substitution_placeholder(literal: &str) -> bool {
    let inner = literal.trim_matches(|c| c == '"' || c == '\'');
    inner.len() > 2
        && inner.starts_with('@')
        && inner.ends_with('@')
        && inner[1..inner.len() - 1]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

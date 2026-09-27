//! Tree-sitter helpers the syntactic templates share, driven by [`Rules`].

use tree_sitter::Node;

use super::rules::Rules;

pub(super) fn text<'s>(node: Node<'_>, source: &'s str) -> &'s str {
    source.get(node.byte_range()).unwrap_or("")
}

/// `(line, col)` of a node's start, as the index records call sites
/// (1-based line, 0-based byte column).
pub(super) fn start(node: Node<'_>) -> (u32, u32) {
    let point = node.start_position();
    (point.row as u32 + 1, point.column as u32)
}

pub(super) fn end(node: Node<'_>) -> (u32, u32) {
    let point = node.end_position();
    (point.row as u32 + 1, point.column as u32)
}

/// Named children, comments left out.
pub(super) fn children<'t>(rules: &Rules, node: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|child| !rules.comments.contains(&child.kind()))
        .collect()
}

/// The last named, non-comment child.
pub(super) fn last_child<'t>(rules: &Rules, node: Node<'t>) -> Option<Node<'t>> {
    children(rules, node).pop()
}

pub(super) fn first_child<'t>(rules: &Rules, node: Node<'t>) -> Option<Node<'t>> {
    children(rules, node).into_iter().next()
}

/// A statement ending in `;`.
pub(super) fn ends_with_semicolon(node: Node<'_>) -> bool {
    node.child(node.child_count().saturating_sub(1) as u32)
        .is_some_and(|last| last.kind() == ";")
}

/// Strip wrappers whose value is their operand's (`await`, `?`, parens).
pub(super) fn strip<'t>(rules: &Rules, mut node: Node<'t>) -> Node<'t> {
    while rules.transparent.contains(&node.kind()) {
        match first_child(rules, node) {
            Some(inner) => node = inner,
            None => break,
        }
    }
    node
}

/// The callee as written, compacted: `Default::default`, `self.upload`.
pub(super) fn callee_text(call: Node<'_>, source: &str) -> String {
    call.child_by_field_name("function")
        .map(|function| {
            text(function, source)
                .chars()
                .filter(|ch| !ch.is_whitespace())
                .collect()
        })
        .unwrap_or_default()
}

/// The called name — a method's or path's last segment — as the index
/// names the callee.
pub(super) fn call_name<'s>(rules: &Rules, call: Node<'_>, source: &'s str) -> Option<&'s str> {
    let mut function = call.child_by_field_name("function")?;
    loop {
        let kind = function.kind();
        if let Some((_, inner)) = rules.generics.iter().find(|(k, _)| *k == kind) {
            function = function.child_by_field_name(inner)?;
        } else if let Some((_, _, member)) = rules.members.iter().find(|(k, ..)| *k == kind) {
            function = function.child_by_field_name(member)?;
        } else if let Some((_, name)) = rules.paths.iter().find(|(k, _)| *k == kind) {
            function = function.child_by_field_name(name)?;
        } else {
            break;
        }
    }
    let name = text(function, source);
    name.chars()
        .all(|ch| ch == '_' || ch == '$' || ch.is_alphanumeric())
        .then_some(name)
        .filter(|name| !name.is_empty())
}

/// The receiver of a method call (`x` in `x.m()`).
pub(super) fn receiver<'t>(rules: &Rules, call: Node<'t>) -> Option<Node<'t>> {
    let function = call.child_by_field_name("function")?;
    let (_, object, _) = rules.members.iter().find(|(k, ..)| *k == function.kind())?;
    function.child_by_field_name(object)
}

/// The arguments of a call, comments left out.
pub(super) fn arguments<'t>(rules: &Rules, call: Node<'t>) -> Vec<Node<'t>> {
    call.child_by_field_name("arguments")
        .map(|args| children(rules, args))
        .unwrap_or_default()
}

/// The single argument of a value-wrapping constructor (`Some(x)`, `Ok(x)`).
pub(super) fn wrapped<'t>(rules: &Rules, node: Node<'t>, source: &str) -> Option<Node<'t>> {
    if !rules.calls.contains(&node.kind()) {
        return None;
    }
    let callee = callee_text(node, source);
    if !rules.value_wrappers.contains(&callee.as_str()) {
        return None;
    }
    match arguments(rules, node).as_slice() {
        [only] => Some(*only),
        _ => None,
    }
}

/// A constant value: a literal, `None`, `()`, `Default::default()`,
/// `vec![]`, `[]`, or a wrapper of one (`Ok(())`, `Some(false)`).
pub(super) fn is_constant(rules: &Rules, node: Node<'_>, source: &str) -> bool {
    let node = strip(rules, node);
    let kind = node.kind();
    if rules.literals.contains(&kind) {
        return true;
    }
    if kind == "identifier" {
        return rules.constant_words.contains(&text(node, source));
    }
    if rules.empty_collections.contains(&kind) {
        return children(rules, node).is_empty();
    }
    if kind == "macro_invocation" {
        let name = node
            .child_by_field_name("macro")
            .map(|name| text(name, source))
            .unwrap_or("");
        return rules.constant_macros.contains(&name)
            && node
                .named_child(node.named_child_count().saturating_sub(1) as u32)
                .is_some_and(|tokens| tokens.named_child_count() == 0);
    }
    if rules.calls.contains(&kind) {
        if let Some(inner) = wrapped(rules, node, source) {
            return is_constant(rules, inner, source);
        }
        return rules
            .constant_calls
            .contains(&callee_text(node, source).as_str())
            && arguments(rules, node).is_empty();
    }
    rules.constant_words.contains(&text(node, source))
}

/// The value a block, arm body or expression ends with.
#[derive(Debug, Clone, Copy)]
pub(super) enum Value<'t> {
    Expr(Node<'t>),
    /// Evaluates to nothing (`()`, a bare `return`).
    Unit,
    /// Yields no value here (a `case` that breaks or falls through).
    Absent,
}

/// What `node` evaluates to, through wrappers, blocks and `return`.
pub(super) fn resolve<'t>(rules: &Rules, node: Node<'t>) -> Value<'t> {
    let node = strip(rules, node);
    if rules.blocks.contains(&node.kind()) {
        return final_value(rules, node, None);
    }
    if rules.returns.contains(&node.kind()) {
        return match first_child(rules, node) {
            Some(value) => resolve(rules, value),
            None => Value::Unit,
        };
    }
    Value::Expr(node)
}

/// The value the statements of `container` end with; `skip` is a child that
/// is not a statement (a `case`'s label).
pub(super) fn final_value<'t>(
    rules: &Rules,
    container: Node<'t>,
    skip: Option<Node<'t>>,
) -> Value<'t> {
    let Some(last) = last_child(rules, container) else {
        return if rules.block_without_tail_is_unit {
            Value::Unit
        } else {
            Value::Absent
        };
    };
    if Some(last) == skip {
        return Value::Absent;
    }
    let kind = last.kind();
    if rules.returns.contains(&kind) || rules.blocks.contains(&kind) {
        return resolve(rules, last);
    }
    if rules.statements.contains(&kind) {
        let inner = first_child(rules, last);
        if rules.statement_needs_semicolon && !ends_with_semicolon(last) {
            return inner.map_or(Value::Unit, |inner| resolve(rules, inner));
        }
        if let Some(inner) = inner.filter(|inner| rules.returns.contains(&inner.kind())) {
            return resolve(rules, inner);
        }
        return no_tail(rules);
    }
    if rules.block_without_tail_is_unit && !is_declaration(kind) {
        // An expression in tail position (Rust).
        return resolve(rules, last);
    }
    no_tail(rules)
}

fn no_tail(rules: &Rules) -> Value<'static> {
    if rules.block_without_tail_is_unit {
        Value::Unit
    } else {
        Value::Absent
    }
}

fn is_declaration(kind: &str) -> bool {
    kind.ends_with("_declaration") || kind.ends_with("_item") || kind == "empty_statement"
}

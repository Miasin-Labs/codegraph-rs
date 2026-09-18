//! Method-call receivers: which part of a call's callee expression is the
//! object the method is invoked on.

use tree_sitter::Node;

/// `(member-access node kind, field holding the object)` per grammar. The
/// kinds are distinct across the lowered grammars, so one table serves all:
/// Rust `field_expression`, Python `attribute`, TypeScript/JavaScript
/// `member_expression`, Go `selector_expression`.
const MEMBER_ACCESS: &[(&str, &str)] = &[
    ("field_expression", "value"),
    ("attribute", "object"),
    ("member_expression", "object"),
    ("selector_expression", "operand"),
];

/// The receiver expression of a call whose callee is `function`: `obj` in
/// `obj.m(x)` / `obj.m::<T>(x)`, `None` for a bare or path call (`f(x)`,
/// `Foo::new(x)`).
///
/// Go's `pkg.Func(x)` and Python's `module.func(x)` also yield a receiver
/// (`pkg`, `module`): syntax cannot tell a package from a value. Consumers
/// that know the callee drop a receiver the callee does not take.
pub(super) fn method_receiver(function: Node<'_>) -> Option<Node<'_>> {
    let access = match function.kind() {
        // Rust turbofish: `obj.m::<T>` wraps the field access.
        "generic_function" => function.child_by_field_name("function")?,
        _ => function,
    };
    let (_, object_field) = MEMBER_ACCESS
        .iter()
        .find(|(kind, _)| *kind == access.kind())?;
    access.child_by_field_name(object_field)
}

/// Comments are named children in every lowered grammar; skipping them keeps
/// argument and parameter positions aligned.
pub(super) fn is_comment(node: Node<'_>) -> bool {
    matches!(node.kind(), "comment" | "line_comment" | "block_comment")
}

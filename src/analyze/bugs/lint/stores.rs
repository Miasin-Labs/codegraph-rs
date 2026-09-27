//! `dead-store`: a local is given a value, then given another in the same
//! block with no read in between — the first value is lost.
//!
//! Only straight-line statements of one block are followed. Anything read
//! between the stores (any mention of the name, format strings included) or
//! in the second store's value keeps the first alive. The walk forgets every
//! pending store at a loop, label or jump (a `continue` reaches the loop
//! head, where the value may be read) and at an attribute (`#[cfg]` makes
//! either store optional). A block inside a `try` (or Python `with`) is
//! skipped: a throw between the stores may reach a handler that reads the
//! first value. The name must be a local of the enclosing function that no
//! closure or nested function mentions and whose address is never taken;
//! `_`-prefixed and constant-cased names are skipped. A first value that is
//! a default (`0`, `""`, `None`, `null`, `[]`, `Vec::new()`, `new T(…)`, a
//! constant) is the "declare, then assign" idiom and is not
//! reported. Rust `let x` again shadows: a new variable, not a store.

use std::collections::HashMap;

use tree_sitter::Node;

use super::rules::StoreShape;
use super::syntax::{
    enclosing_function,
    is_constant_name,
    is_pure,
    named_children,
    path_roots,
    position,
    text,
    walk,
    words,
};
use super::{Ctx, DEAD_STORE};

/// One statement's stores.
struct Store<'t> {
    targets: Vec<Node<'t>>,
    value: Option<Node<'t>>,
    /// Rust `let`: a new variable under the same name.
    shadows: bool,
}

pub(super) fn check_block(block: Node<'_>, ctx: &mut Ctx<'_>) {
    let rules = ctx.rules;
    let source = ctx.source;
    let Some(function) = enclosing_function(block, rules) else {
        return;
    };
    // Inside a `try`: a throw may reach a handler that reads the variable.
    let mut current = block.parent();
    while let Some(n) = current {
        if n.id() == function.id() {
            break;
        }
        if rules.tries.contains(&n.kind()) {
            return;
        }
        current = n.parent();
    }

    // name → (line, col) of the store not yet read.
    let mut pending: HashMap<&str, (u32, u32)> = HashMap::new();
    let mut found: Vec<(Node<'_>, &str, (u32, u32))> = Vec::new();
    for statement in named_children(block, rules) {
        let kind = statement.kind();
        if rules.attributes.contains(&kind) || (!pending.is_empty() && is_barrier(statement, ctx)) {
            pending.clear();
            continue;
        }
        let Some(store) = store_of(statement, ctx) else {
            if !pending.is_empty() {
                let read = words(statement, rules, source);
                pending.retain(|name, _| !read.contains(name));
            }
            continue;
        };
        if let Some(value) = store.value.filter(|_| !pending.is_empty()) {
            let read = words(value, rules, source);
            pending.retain(|name, _| !read.contains(name));
        }
        let default_value = store.value.is_none_or(|value| is_default(value, ctx));
        for target in store.targets {
            let name = text(target, source);
            if store.shadows {
                pending.remove(name);
            } else if let Some(first) = pending.remove(name) {
                found.push((target, name, first));
            }
            if !default_value && eligible(name, function, ctx) {
                pending.insert(name, position(target));
            }
        }
    }
    for (target, name, (line, col)) in found {
        let (again, _) = position(target);
        let message = format!(
            "the value stored in `{name}` at line {line} is overwritten at line {again} \
             before anything reads it"
        );
        ctx.out.push(super::Raw {
            rule: "dead-store",
            line,
            col,
            message,
            confidence: DEAD_STORE,
            evidence: vec![(again, format!("`{name}` is assigned again here"))],
        });
    }
}

/// The statement holds a loop, label or jump.
fn is_barrier(statement: Node<'_>, ctx: &Ctx<'_>) -> bool {
    let rules = ctx.rules;
    let mut barrier = false;
    walk(statement, |n| {
        let kind = n.kind();
        if rules.loops.contains(&kind)
            || rules.jumps.contains(&kind)
            || rules.opaque.contains(&kind)
            || rules.exits.iter().any(|e| {
                // `return` reads Go's named results; `yield` hands control
                // to code that may read anything.
                *e == kind && (kind.starts_with("yield") || kind.starts_with("return"))
            })
        {
            barrier = true;
        }
        !barrier && !rules.functions.contains(&kind)
    });
    barrier
}

fn store_of<'t>(statement: Node<'t>, ctx: &Ctx<'_>) -> Option<Store<'t>> {
    let rules = ctx.rules;
    for shape in rules.stores {
        match *shape {
            StoreShape::Assign {
                wrapper,
                kind,
                left,
                right,
                operator,
            } => {
                let node = if wrapper.is_empty() {
                    statement
                } else if statement.kind() == wrapper {
                    match named_children(statement, rules).as_slice() {
                        [inner] => *inner,
                        _ => continue,
                    }
                } else {
                    continue;
                };
                if node.kind() != kind {
                    continue;
                }
                if operator
                    && node
                        .child_by_field_name("operator")
                        .is_none_or(|op| text(op, ctx.source) != "=")
                {
                    return None;
                }
                let targets = plain_targets(node.child_by_field_name(left)?, ctx)?;
                let value = node.child_by_field_name(right)?;
                if rules
                    .stores
                    .iter()
                    .any(|s| matches!(s, StoreShape::Assign { kind: k, .. } if *k == value.kind()))
                {
                    // `a = b = c`: a chain, not followed.
                    return None;
                }
                return Some(Store {
                    targets,
                    value: Some(value),
                    shadows: false,
                });
            }
            StoreShape::Declare {
                kind,
                declarator,
                name,
                value,
                shadows,
            } => {
                if statement.kind() != kind {
                    continue;
                }
                let node = if declarator.is_empty() {
                    statement
                } else {
                    let mut cursor = statement.walk();
                    let declarators: Vec<Node<'t>> = statement
                        .named_children(&mut cursor)
                        .filter(|c| c.kind() == declarator)
                        .collect();
                    match declarators.as_slice() {
                        [one] => *one,
                        _ => return None,
                    }
                };
                let target = node.child_by_field_name(name)?;
                if !rules.idents.contains(&target.kind()) {
                    return None;
                }
                let value = node.child_by_field_name(value);
                if value.is_none() && !shadows {
                    // `let x;`: declares, stores nothing.
                    return None;
                }
                return Some(Store {
                    targets: vec![target],
                    value,
                    shadows,
                });
            }
        }
    }
    None
}

/// The target is one or more plain variables (`x`, Go `x, err`), or `None`.
fn plain_targets<'t>(left: Node<'t>, ctx: &Ctx<'_>) -> Option<Vec<Node<'t>>> {
    let rules = ctx.rules;
    if rules.idents.contains(&left.kind()) {
        return Some(vec![left]);
    }
    // A list of names (Go `x, err`, Python `a, b`).
    if !matches!(left.kind(), "expression_list" | "pattern_list") {
        return None;
    }
    let parts = named_children(left, rules);
    if parts.is_empty() || !parts.iter().all(|p| rules.idents.contains(&p.kind())) {
        return None;
    }
    Some(parts)
}

/// A local of `function` whose value only this function's straight-line
/// code can read.
fn eligible(name: &str, function: Node<'_>, ctx: &mut Ctx<'_>) -> bool {
    if name.starts_with('_') || is_constant_name(name) || ctx.rules.receivers.contains(&name) {
        return false;
    }
    let rules = ctx.rules;
    let scope = ctx.scopes.get(function, rules, ctx.source);
    !scope.escaped.contains(name) && (rules.idents_are_locals || scope.declared.contains(name))
}

/// A placeholder first value: literal, constant, empty collection,
/// zero-argument constructor or call.
fn is_default(value: Node<'_>, ctx: &Ctx<'_>) -> bool {
    let rules = ctx.rules;
    let source = ctx.source;
    // Go `x := 0` keeps its value in a one-element list; `{} as T`, `(T) x`
    // and parentheses only restate the type.
    let mut value = value;
    loop {
        let parts = named_children(value, rules);
        match (value.kind(), parts.as_slice()) {
            ("expression_list", [only]) | ("parenthesized_expression", [only]) => value = *only,
            (
                "as_expression"
                | "satisfies_expression"
                | "type_assertion"
                | "non_null_expression"
                | "type_cast_expression"
                | "cast_expression",
                [first, ..],
            ) => {
                // The value is the expression part, not the type.
                let inner = value.child_by_field_name("value").unwrap_or(*first);
                if inner.id() == value.id() {
                    break;
                }
                value = inner;
            }
            _ => break,
        }
    }
    // A literal of placeholders: `{ items: [], next: undefined }`.
    let mut calls = false;
    let mut names_value = false;
    walk(value, |n| {
        let kind = n.kind();
        if rules.calls.contains(&kind) || rules.macro_kind == Some(kind) {
            calls = true;
        }
        if n.child_count() == 0
            && rules.idents.contains(&kind)
            && !matches!(text(n, source), "undefined" | "None" | "nil" | "null")
        {
            names_value = true;
        }
        !calls && !names_value
    });
    if !calls && !names_value {
        return true;
    }
    if is_pure(value, rules, source)
        && path_roots(value, rules, source)
            .iter()
            .all(|root| is_constant_name(root) || matches!(*root, "undefined" | "None"))
    {
        return true;
    }
    let flat: String = text(value, source).split_whitespace().collect();
    if flat.len() > 80 {
        return false;
    }
    let bare = flat.trim_end_matches(';');
    // A constructor: `new Label(parent, …)` registers itself with its
    // parent, and the variable is only a handle, reused on purpose.
    if text(value, source).trim_start().starts_with("new ") {
        return true;
    }
    if bare.ends_with("[]")
        || bare.ends_with("{}")
        || bare.starts_with("make(")
        || matches!(bare, "None" | "null" | "nil" | "undefined")
    {
        return true;
    }
    // Zero-argument constructors: `Vec::new()`, `T::default()`, `new Map()`,
    // `dict()`, `Foo()` — not any zero-argument call (`first()` computes).
    let Some(callee) = bare.strip_suffix("()") else {
        return false;
    };
    let callee = callee.split('<').next().unwrap_or(callee);
    let last = callee.rsplit(['.', ':']).next().unwrap_or(callee);
    last.starts_with(|c: char| c.is_uppercase())
        || last.starts_with("empty")
        || matches!(
            last,
            "new" | "default" | "of" | "set" | "dict" | "list" | "tuple" | "frozenset" | "object"
        )
}

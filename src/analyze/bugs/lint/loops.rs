//! `loop-no-progress`: a loop runs while a condition holds, the condition
//! reads only variables nothing in the body can change, and nothing in the
//! body leaves the loop — once entered, it never ends.
//!
//! "Can change" is read generously, so the rule only fires where the syntax
//! rules every change out: a variable is changeable if the body assigns,
//! increments, declares or borrows (`&mut`, Go `&`) it, names it in any
//! call or macro (a method may mutate its receiver, a callee its `&mut`
//! argument), or a closure anywhere in the function mentions it. Outside
//! Rust a variable must be a declared local (fields, globals and `this` can
//! change behind any call or thread), a condition through a field path is
//! skipped (another thread may own the object), and a `CONSTANT` global
//! gives up on any call in the body. The body must hold no break, return,
//! throw, `?`, yield, await, labelled continue, exit/panic call, `unsafe`
//! block or unknown macro.

use std::collections::HashSet;

use tree_sitter::Node;

use super::rules::{EXIT_CALLS, QUIET_MACROS};
use super::syntax::{
    enclosing_function,
    has_child_kind,
    has_field_path,
    is_atom,
    is_constant_name,
    is_pure,
    path_roots,
    pick,
    split_words,
    text,
};
use super::{Ctx, LOOP_NO_PROGRESS};

pub(super) fn check(node: Node<'_>, ctx: &mut Ctx<'_>) {
    let rules = ctx.rules;
    let source = ctx.source;
    let Some(shape) = rules.cond_loops.iter().find(|l| l.kind == node.kind()) else {
        return;
    };
    let Some(condition) = pick(node, shape.condition, rules) else {
        return;
    };
    let Some(body) = node.child_by_field_name(shape.body) else {
        return;
    };
    if condition.id() == body.id() || !is_pure(condition, rules, source) {
        return;
    }
    if !rules.idents_are_locals && has_field_path(condition, rules) {
        // A field can be changed by another thread or any aliasing call.
        return;
    }
    let roots = path_roots(condition, rules, source);
    if roots.is_empty() {
        // Literal conditions: `while true` is infinite by design.
        return;
    }
    let function = enclosing_function(node, rules);
    if function.is_none() && !rules.idents_are_locals {
        return;
    }
    let mut watched: HashSet<&str> = HashSet::new();
    let mut reads_globals = false;
    {
        let scope = function.map(|f| ctx.scopes.get(f, rules, source));
        for root in roots {
            if scope.is_some_and(|s| s.escaped.contains(root)) {
                return;
            }
            if rules.receivers.contains(&root) {
                if !rules.idents_are_locals {
                    return;
                }
                watched.insert(root);
            } else if rules.idents_are_locals {
                if !is_constant_name(root) {
                    watched.insert(root);
                }
            } else if scope.is_some_and(|s| s.declared.contains(root)) {
                watched.insert(root);
            } else if is_constant_name(root) {
                reads_globals = true;
            } else {
                return;
            }
        }
    }
    if watched.is_empty() {
        return;
    }

    let mut scan = BodyScan {
        ctx,
        watched: &watched,
        blocked: false,
        calls: false,
    };
    scan.visit(body, 0);
    let (blocked, calls) = (scan.blocked, scan.calls);
    if blocked {
        return;
    }
    if calls && reads_globals {
        return;
    }

    let mut names: Vec<&str> = watched.into_iter().collect();
    names.sort_unstable();
    let names = names
        .iter()
        .map(|n| format!("`{n}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let message = format!(
        "loop on `{}` never ends once entered: nothing in its body changes {names}, and \
         nothing leaves the loop",
        ctx.snippet(condition)
    );
    ctx.report(
        "loop-no-progress",
        node,
        LOOP_NO_PROGRESS,
        message,
        Vec::new(),
    );
}

const IN_CALL: u8 = 1;
const IN_TARGET: u8 = 2;
const IN_CLOSURE: u8 = 4;

struct BodyScan<'a, 'c, 's> {
    ctx: &'a Ctx<'c>,
    watched: &'a HashSet<&'s str>,
    /// Something may change a watched variable or leave the loop.
    blocked: bool,
    /// The body calls something.
    calls: bool,
}

impl BodyScan<'_, '_, '_> {
    fn visit(&mut self, node: Node<'_>, flags: u8) {
        crate::ensure_sufficient_stack(|| self.visit_inner(node, flags));
    }

    fn visit_inner(&mut self, node: Node<'_>, mut flags: u8) {
        if self.blocked {
            return;
        }
        let rules = self.ctx.rules;
        let source = self.ctx.source;
        let kind = node.kind();
        if rules.comments.contains(&kind) {
            return;
        }
        if rules.exits.contains(&kind)
            || rules.opaque.contains(&kind)
            || (rules.continues.contains(&kind) && node.named_child_count() > 0)
        {
            self.blocked = true;
            return;
        }
        if rules.functions.contains(&kind) {
            flags |= IN_CLOSURE;
        }
        if rules.calls.contains(&kind) {
            self.calls = true;
            flags |= IN_CALL;
            if is_exit_call(node, source) {
                self.blocked = true;
                return;
            }
        }
        if rules.macro_kind == Some(kind) {
            self.calls = true;
            flags |= IN_CALL;
            let name = node
                .child_by_field_name("macro")
                .map(|m| text(m, source))
                .unwrap_or("");
            let last = name.rsplit("::").next().unwrap_or(name);
            if !QUIET_MACROS.contains(&last) {
                self.blocked = true;
                return;
            }
        }
        if is_atom(node, source) {
            if flags != 0 && split_words(text(node, source)).any(|w| self.watched.contains(w)) {
                self.blocked = true;
            }
            return;
        }
        let mutation = rules
            .mutations
            .iter()
            .find(|m| m.kind == kind && m.marker.is_none_or(|marker| has_child_kind(node, marker)));
        let mut cursor = node.walk();
        let children: Vec<(Node<'_>, Option<&str>)> = {
            let mut out = Vec::new();
            if cursor.goto_first_child() {
                loop {
                    out.push((cursor.node(), cursor.field_name()));
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
            out
        };
        for (child, field) in children {
            let target = mutation.is_some_and(|m| m.target.is_empty() || field == Some(m.target));
            self.visit(child, if target { flags | IN_TARGET } else { flags });
        }
    }
}

/// `exit(…)`, `os.Exit(…)`, `panic(…)`, `log.Fatal(…)`: the program ends.
fn is_exit_call(node: Node<'_>, source: &str) -> bool {
    let call = text(node, source);
    let head = call.split('(').next().unwrap_or("");
    let head = head.get(..head.len().min(200)).unwrap_or(head);
    let last = head
        .rsplit(['.', ':'])
        .next()
        .unwrap_or(head)
        .trim_start_matches("new ")
        .trim();
    EXIT_CALLS.contains(&last)
}

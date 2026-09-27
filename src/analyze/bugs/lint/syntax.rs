//! Syntax helpers shared by the rules: walking, tokens, purity, the roots of
//! a variable path, and what a function scope declares.

use std::collections::{HashMap, HashSet};

use tree_sitter::Node;

use super::rules::{Pick, Rules};

/// Visit `root` and its descendants in document order; `visit` returns
/// whether to descend into the node it was given. Iterative: depth is
/// bounded by the input, not the stack.
pub(super) fn walk<'t>(root: Node<'t>, mut visit: impl FnMut(Node<'t>) -> bool) {
    let mut cursor = root.walk();
    loop {
        let descend = visit(cursor.node());
        if descend && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}

pub(super) fn text<'s>(node: Node<'_>, source: &'s str) -> &'s str {
    source.get(node.byte_range()).unwrap_or("")
}

/// The tokens of `node` with comments dropped: what two pieces of code are
/// compared by, whitespace and comments aside.
pub(super) fn tokens<'s>(node: Node<'_>, rules: &Rules, source: &'s str) -> Vec<&'s str> {
    let mut out = Vec::new();
    walk(node, |n| {
        if rules.comments.contains(&n.kind()) {
            return false;
        }
        if is_atom(n, source) {
            let t = text(n, source);
            if !t.is_empty() {
                out.push(t);
            }
            return false;
        }
        true
    });
    out
}

/// A node read as one token: a leaf, or a node whose text is not all in
/// its children (Python's `format_specifier` keeps `+.2g` to itself, some
/// grammars hide string contents) — descending would lose that text.
pub(super) fn is_atom(node: Node<'_>, source: &str) -> bool {
    if node.child_count() == 0 {
        return true;
    }
    let mut cursor = node.walk();
    let mut at = node.start_byte();
    let mut hidden = false;
    for child in node.children(&mut cursor) {
        let gap = source.get(at..child.start_byte()).unwrap_or("");
        if !gap.trim().is_empty() {
            hidden = true;
            break;
        }
        at = child.end_byte();
    }
    hidden
        || !source
            .get(at..node.end_byte())
            .unwrap_or("")
            .trim()
            .is_empty()
}

/// Whether two nodes have the same tokens, stopping at the first
/// difference (the common case costs a few tokens, not the subtree).
pub(super) fn same_tokens(a: Node<'_>, b: Node<'_>, rules: &Rules, source: &str) -> bool {
    if a.kind() != b.kind() {
        return false;
    }
    let mut left = Leaves::new(a, rules, source);
    let mut right = Leaves::new(b, rules, source);
    loop {
        match (left.next(), right.next()) {
            (None, None) => return true,
            (Some(x), Some(y)) if text(x, source) == text(y, source) => {}
            _ => return false,
        }
    }
}

/// Non-comment leaves of a subtree, lazily.
struct Leaves<'t, 'r> {
    cursor: tree_sitter::TreeCursor<'t>,
    rules: &'r Rules,
    source: &'r str,
    done: bool,
    started: bool,
}

impl<'t, 'r> Leaves<'t, 'r> {
    fn new(root: Node<'t>, rules: &'r Rules, source: &'r str) -> Self {
        Self {
            cursor: root.walk(),
            rules,
            source,
            done: false,
            started: false,
        }
    }

    /// Move to the next node in document order (not descending into
    /// comments); false at the end.
    fn advance(&mut self) -> bool {
        if !self.started {
            self.started = true;
            return true;
        }
        let node = self.cursor.node();
        if !self.rules.comments.contains(&node.kind())
            && !is_atom(node, self.source)
            && self.cursor.goto_first_child()
        {
            return true;
        }
        loop {
            if self.cursor.goto_next_sibling() {
                return true;
            }
            if !self.cursor.goto_parent() {
                return false;
            }
        }
    }
}

impl<'t> Iterator for Leaves<'t, '_> {
    type Item = Node<'t>;

    fn next(&mut self) -> Option<Node<'t>> {
        while !self.done {
            if !self.advance() {
                self.done = true;
                break;
            }
            let node = self.cursor.node();
            if !self.rules.comments.contains(&node.kind()) && is_atom(node, self.source) {
                return Some(node);
            }
        }
        None
    }
}

/// The named children of `node`, comments skipped.
pub(super) fn named_children<'t>(node: Node<'t>, rules: &Rules) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|child| !rules.comments.contains(&child.kind()))
        .collect()
}

pub(super) fn pick<'t>(node: Node<'t>, how: Pick, rules: &Rules) -> Option<Node<'t>> {
    match how {
        Pick::Field(name) => node.child_by_field_name(name),
        Pick::Nth(n) => named_children(node, rules).into_iter().nth(n),
        Pick::FirstExcept(kinds) => named_children(node, rules)
            .into_iter()
            .find(|child| !kinds.contains(&child.kind())),
    }
}

/// `node` without enclosing parentheses.
pub(super) fn unparen<'t>(mut node: Node<'t>, rules: &Rules) -> Node<'t> {
    while rules.parens.contains(&node.kind()) {
        match named_children(node, rules).as_slice() {
            [inner] => node = *inner,
            _ => break,
        }
    }
    node
}

/// Whether evaluating `node` cannot change state or depend on a call:
/// variables, field paths, literals, constants and operators over them.
pub(super) fn is_pure(node: Node<'_>, rules: &Rules, source: &str) -> bool {
    crate::ensure_sufficient_stack(|| {
        let kind = node.kind();
        if node.child_count() == 0
            || rules.literals.contains(&kind)
            || rules.constant_paths.contains(&kind)
        {
            return true;
        }
        if let Some(&(_, object)) = rules.fields.iter().find(|(k, _)| *k == kind) {
            return node
                .child_by_field_name(object)
                .is_some_and(|object| is_pure(object, rules, source));
        }
        if !rules.transparent.contains(&kind) {
            return false;
        }
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
        children.into_iter().all(|child| {
            if rules.comments.contains(&child.kind()) {
                true
            } else if child.is_named() {
                is_pure(child, rules, source)
            } else {
                !rules.impure_ops.contains(&text(child, source))
            }
        })
    })
}

/// The variables a pure expression reads: the leftmost name of each path
/// (`a.b.c` → `a`), constants and member names left out.
pub(super) fn path_roots<'s>(node: Node<'_>, rules: &Rules, source: &'s str) -> Vec<&'s str> {
    let mut out = Vec::new();
    walk(node, |n| {
        let kind = n.kind();
        if rules.comments.contains(&kind) || rules.constant_paths.contains(&kind) {
            return false;
        }
        if let Some(&(_, object)) = rules.fields.iter().find(|(k, _)| *k == kind) {
            if let Some(object) = n.child_by_field_name(object) {
                out.extend(path_roots(object, rules, source));
            }
            return false;
        }
        if n.child_count() == 0 && rules.idents.contains(&kind) {
            out.push(text(n, source));
        }
        true
    });
    out
}

/// Whether the expression goes through a field (`a.b`).
pub(super) fn has_field_path(node: Node<'_>, rules: &Rules) -> bool {
    let mut found = false;
    walk(node, |n| {
        if rules.fields.iter().any(|(k, _)| *k == n.kind()) {
            found = true;
        }
        !found
    });
    found
}

/// Every identifier-like word in the text of `node`'s leaves (string
/// contents included: `format!("{x}")` reads `x`).
pub(super) fn words<'s>(node: Node<'_>, rules: &Rules, source: &'s str) -> HashSet<&'s str> {
    let mut out = HashSet::new();
    walk(node, |n| {
        if rules.comments.contains(&n.kind()) {
            return false;
        }
        if is_atom(n, source) {
            out.extend(split_words(text(n, source)));
            return false;
        }
        true
    });
    out
}

pub(super) fn split_words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
        .filter(|w| !w.is_empty())
}

/// `SCREAMING_CASE`: a constant by convention.
pub(super) fn is_constant_name(name: &str) -> bool {
    name.chars().any(|c| c.is_ascii_uppercase())
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// The innermost function scope around `node`.
pub(super) fn enclosing_function<'t>(node: Node<'t>, rules: &Rules) -> Option<Node<'t>> {
    let mut current = node.parent();
    while let Some(n) = current {
        if rules.functions.contains(&n.kind()) {
            return Some(n);
        }
        current = n.parent();
    }
    None
}

/// What a function scope declares, and which of its names escape it.
#[derive(Default)]
pub(super) struct Scope {
    /// Parameters and local declarations (nested functions excluded).
    pub declared: HashSet<String>,
    /// Names a nested function or closure mentions, names whose address is
    /// taken, `global`/`nonlocal` names: something other than straight-line
    /// code of this scope may read or write them.
    pub escaped: HashSet<String>,
}

/// Scopes computed once per function node.
#[derive(Default)]
pub(super) struct Scopes {
    by_id: HashMap<usize, Scope>,
}

impl Scopes {
    pub fn get(&mut self, function: Node<'_>, rules: &Rules, source: &str) -> &Scope {
        self.by_id
            .entry(function.id())
            .or_insert_with(|| scope_of(function, rules, source))
    }
}

fn scope_of(function: Node<'_>, rules: &Rules, source: &str) -> Scope {
    let mut scope = Scope::default();
    let idents_of = |node: Node<'_>, into: &mut HashSet<String>| {
        walk(node, |n| {
            if n.child_count() == 0 && rules.idents.contains(&n.kind()) {
                into.insert(text(n, source).to_string());
            }
            true
        });
    };
    for field in rules.param_fields {
        let mut cursor = function.walk();
        for param in function.children_by_field_name(field, &mut cursor) {
            idents_of(param, &mut scope.declared);
        }
    }
    let mut body_cursor = function.walk();
    let children: Vec<Node<'_>> = function.children(&mut body_cursor).collect();
    for child in children {
        walk(child, |n| {
            let kind = n.kind();
            if rules.functions.contains(&kind) {
                // A nested scope: whatever it names may run at any call.
                walk(n, |inner| {
                    if is_atom(inner, source) {
                        scope
                            .escaped
                            .extend(split_words(text(inner, source)).map(str::to_string));
                        return false;
                    }
                    true
                });
                return false;
            }
            if rules.non_locals.contains(&kind) {
                idents_of(n, &mut scope.escaped);
                return false;
            }
            if let Some(&(_, field)) = rules.declarations.iter().find(|(k, _)| *k == kind) {
                let mut cursor = n.walk();
                for part in n.children_by_field_name(field, &mut cursor) {
                    // A declarator with its own entry (C's `init_declarator`
                    // in a `declaration`) names its variable there: its
                    // initializer declares nothing.
                    if rules.declarations.iter().any(|(k, _)| *k == part.kind()) {
                        continue;
                    }
                    idents_of(part, &mut scope.declared);
                }
            }
            let address_taken = rules.mutations.iter().any(|mutation| {
                mutation.kind == kind && mutation.marker.is_some_and(|m| has_child_kind(n, m))
            });
            if address_taken {
                idents_of(n, &mut scope.escaped);
            }
            true
        });
    }
    scope
}

pub(super) fn has_child_kind(node: Node<'_>, kind: &str) -> bool {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).any(|child| child.kind() == kind);
    found
}

/// 1-based line and 0-based column of `node`.
pub(super) fn position(node: Node<'_>) -> (u32, u32) {
    let point = node.start_position();
    (point.row as u32 + 1, point.column as u32)
}

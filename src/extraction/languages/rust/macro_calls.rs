//! Calls written inside Rust macro invocations.
//!
//! tree-sitter-rust keeps a macro invocation's arguments as an unparsed
//! `token_tree`, so `assert_eq!(helper(), 1)` holds no `call_expression` and
//! the call to `helper` never became an edge. Macro *expansion* stays out of
//! scope; this scans the tokens (nested trees included) for the call shapes an
//! argument can spell, and names each the way `calls.rs` names the equivalent
//! parsed call:
//!
//! - `f(…)` → `f`
//! - `a::b::c(…)` → `a::b::c`
//! - `recv.m(…)` → `recv.m` when `recv` is a plain identifier, bare `m` when
//!   it is `self` or any longer expression (`a.b().m()`, `x[0].m()`); the
//!   longer ones are marked as having dropped their receiver
//!
//! Attributes (`#[derive(Debug)]`), nested `macro_rules!` definitions, the
//! pattern half of `matches!`, `$`-prefixed metavariables, and item
//! declarations (`fn f(`, `struct S(`) are not calls and are skipped.

use std::ops::Range;

use crate::extraction::tree_sitter_helpers::get_node_text;
use crate::extraction::tree_sitter_types::{SyntaxNode, TokenCall};

/// `Fn(u32) -> u32` in a type position reads like a call but names a trait.
const CALL_SHAPED_TYPES: &[&str] = &["Fn", "FnMut", "FnOnce"];

/// Tokens after which `name(` declares an item or reads a metavariable
/// instead of calling. A `::` means the path continues through something
/// this scan does not model (`Vec::<u8>::new(`, `<T as Tr>::f(`), so the
/// callee cannot be named faithfully.
const NON_CALL_PREDECESSORS: &[&str] = &["$", "::", "fn", "struct", "enum", "union", "trait"];

/// Macros whose second argument is a pattern (`matches!(v, Some(_) if g)`),
/// where a tuple-struct pattern looks like a call.
const PATTERN_MACROS: &[&str] = &["matches", "assert_matches", "debug_assert_matches"];

/// Every call shape in `invocation`'s token trees, in source order.
pub(super) fn macro_invocation_calls(invocation: SyntaxNode<'_>, source: &str) -> Vec<TokenCall> {
    let Some(arguments) = tokens_of(invocation)
        .into_iter()
        .find(|child| child.kind() == "token_tree")
    else {
        return Vec::new();
    };
    let name = invocation
        .child_by_field_name("macro")
        .map(|path| last_segment(get_node_text(path, source)));

    // Iterative: nesting depth is bounded by the input, not the stack.
    let mut calls = Vec::new();
    let mut pending = vec![(arguments, name)];
    while let Some((tree, macro_name)) = pending.pop() {
        let tokens = tokens_of(tree);
        let pattern = pattern_span(&tokens, macro_name);
        for (index, token) in tokens.iter().enumerate() {
            if token.kind() != "token_tree"
                || pattern.contains(&index)
                || is_attribute(&tokens, index)
                || defines_macro(&tokens, index, source)
            {
                continue;
            }
            if opens_with(*token, "(") {
                calls.extend(call_before(&tokens, index, source));
            }
            pending.push((*token, invoked_macro(&tokens, index, source)));
        }
    }
    calls.sort_by_key(|call| (call.line, call.column));
    calls
}

/// All children of `node`, anonymous punctuation and keywords included.
fn tokens_of(node: SyntaxNode<'_>) -> Vec<SyntaxNode<'_>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

fn last_segment(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path)
}

fn opens_with(tree: SyntaxNode<'_>, delimiter: &str) -> bool {
    tree.child(0).is_some_and(|open| open.kind() == delimiter)
}

/// The token before `index`, if any.
fn before(tokens: &[SyntaxNode<'_>], index: usize) -> Option<&'static str> {
    index.checked_sub(1).map(|previous| tokens[previous].kind())
}

/// The macro a token tree is the argument list of: `name` in `name!(…)`.
fn invoked_macro<'s>(tokens: &[SyntaxNode<'_>], index: usize, source: &'s str) -> Option<&'s str> {
    (index >= 2 && tokens[index - 1].kind() == "!" && tokens[index - 2].kind() == "identifier")
        .then(|| get_node_text(tokens[index - 2], source))
}

/// `#[…]` / `#![…]`: an attribute, whose arguments are never calls.
fn is_attribute(tokens: &[SyntaxNode<'_>], index: usize) -> bool {
    if !opens_with(tokens[index], "[") {
        return false;
    }
    match before(tokens, index) {
        Some("#") => true,
        Some("!") => before(tokens, index - 1) == Some("#"),
        _ => false,
    }
}

/// The body of `macro_rules! name { … }`: patterns and `$x` templates.
fn defines_macro(tokens: &[SyntaxNode<'_>], index: usize, source: &str) -> bool {
    index >= 3
        && tokens[index - 1].kind() == "identifier"
        && tokens[index - 2].kind() == "!"
        && get_node_text(tokens[index - 3], source) == "macro_rules"
}

/// Token indices holding the pattern of a `matches!`-style macro: from the
/// first top-level `,` up to its `if` guard or next `,`.
fn pattern_span(tokens: &[SyntaxNode<'_>], macro_name: Option<&str>) -> Range<usize> {
    if !macro_name.is_some_and(|name| PATTERN_MACROS.contains(&name)) {
        return 0..0;
    }
    let Some(comma) = tokens.iter().position(|token| token.kind() == ",") else {
        return 0..0;
    };
    let end = tokens[comma + 1..]
        .iter()
        .position(|token| matches!(token.kind(), "if" | ","))
        .map_or(tokens.len(), |offset| comma + 1 + offset);
    comma..end
}

/// A path segment: an identifier, a path root (`crate::f()`, `self::f()`,
/// `super::f()`), or a primitive type (`u32::from(x)`).
fn is_path_segment(token: SyntaxNode<'_>) -> bool {
    matches!(
        token.kind(),
        "identifier" | "crate" | "self" | "super" | "primitive_type"
    )
}

/// Index of the first segment of the `a::b::c` path that ends at `last`.
fn path_start(tokens: &[SyntaxNode<'_>], last: usize) -> usize {
    let mut first = last;
    while first >= 2 && tokens[first - 1].kind() == "::" && is_path_segment(tokens[first - 2]) {
        first -= 2;
    }
    first
}

/// The call whose argument list is the `(`-tree at `open`, if the tokens
/// before it spell a callee.
fn call_before(tokens: &[SyntaxNode<'_>], open: usize, source: &str) -> Option<TokenCall> {
    let last = open.checked_sub(1)?;
    let callee = tokens[last];
    if callee.kind() != "identifier" || CALL_SHAPED_TYPES.contains(&get_node_text(callee, source)) {
        return None;
    }
    let first = path_start(tokens, last);
    match before(tokens, first) {
        Some(".") if first == last => Some(method_call(tokens, first - 1, callee, source)),
        Some(".") => None,
        Some(kind) if NON_CALL_PREDECESSORS.contains(&kind) => None,
        _ => {
            let path: Vec<&str> = tokens[first..=last]
                .iter()
                .step_by(2)
                .map(|segment| get_node_text(*segment, source))
                .collect();
            Some(TokenCall::at(path.join("::"), tokens[first]))
        }
    }
}

/// `recv.m(…)` with the `.` at `dot`. A plain identifier receiver is kept
/// (`recv.m`); `self` and longer receivers (`a.b().m`, `x[0].m`, `$x.m`)
/// leave the bare method name, exactly as `calls.rs` names parsed calls.
fn method_call(
    tokens: &[SyntaxNode<'_>],
    dot: usize,
    method: SyntaxNode<'_>,
    source: &str,
) -> TokenCall {
    let method_name = get_node_text(method, source);
    let receiver = dot.checked_sub(1).map(|index| tokens[index]);
    let longer_receiver = dot >= 1 && matches!(before(tokens, dot - 1), Some("." | "::" | "$"));
    match receiver {
        Some(receiver) if !longer_receiver && receiver.kind() == "identifier" => {
            let receiver_name = get_node_text(receiver, source);
            TokenCall::at(format!("{receiver_name}.{method_name}"), receiver)
        }
        Some(receiver) if !longer_receiver && receiver.kind() == "self" => {
            TokenCall::at(method_name.to_string(), receiver)
        }
        _ => TokenCall::at(method_name.to_string(), method).with_dropped_receiver(),
    }
}

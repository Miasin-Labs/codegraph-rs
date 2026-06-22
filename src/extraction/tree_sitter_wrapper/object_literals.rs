use super::context::*;
use super::extractor::TreeSitterExtractor;
use crate::ensure_sufficient_stack;
use crate::extraction::tree_sitter_helpers::*;
use crate::extraction::tree_sitter_types::*;

impl<'a> TreeSitterExtractor<'a> {
    /// Extract function-valued properties of an object literal as named function
    /// nodes (named by their property key). Shared by the two object-of-functions
    /// shapes in extract_variable: the object as a direct const value, and the
    /// object returned by a store-initializer call. Handles both `key: () => {}` /
    /// `key: function() {}` pairs and method shorthand `key() {}`.
    pub(super) fn extract_object_literal_functions(&mut self, obj: SyntaxNode<'_>) {
        for member in named_children(obj) {
            if member.kind() == "pair" {
                let key = get_child_by_field(member, "key");
                let value = get_child_by_field(member, "value");
                if let (Some(key), Some(value)) = (key, value) {
                    if value.kind() == "arrow_function" || value.kind() == "function_expression" {
                        let name = self.object_key_name(key);
                        self.extract_function(value, Some(&name));
                    }
                }
            } else if member.kind() == "method_definition" {
                // Method shorthand: `{ fetchUser() {...} }`. extract_method deliberately
                // skips object-literal methods, so route through extract_function with an
                // explicit name (method_definition exposes a `body` field, so resolve_body
                // falls through to it and the node spans the full method).
                if let Some(key) = get_child_by_field(member, "name") {
                    let name = self.object_key_name(key);
                    self.extract_function(member, Some(&name));
                }
            }
        }
    }

    /// Property-key text with surrounding quotes stripped (`'foo'` → `foo`).
    pub(super) fn object_key_name(&self, key: SyntaxNode<'_>) -> String {
        let mut text = get_node_text(key, self.source).to_string();
        if text.starts_with('\'') || text.starts_with('"') || text.starts_with('`') {
            text.remove(0);
        }
        if text.ends_with('\'') || text.ends_with('"') || text.ends_with('`') {
            text.pop();
        }
        text
    }

    /// Given a `call_expression` initializer (`create((set, get) => ({...}))`),
    /// find the object literal RETURNED by a function argument — descending through
    /// nested call_expression arguments so middleware wrappers are unwrapped
    /// (`create(persist((set, get) => ({...}), {...}))`, devtools, immer,
    /// subscribeWithSelector). Returns None when no such object is found — the
    /// common case for ordinary call initializers — so this stays cheap and silent
    /// rather than guessing. Keyed purely on AST shape; no library names.
    pub(super) fn find_initializer_returned_object<'t>(
        &self,
        call_node: SyntaxNode<'t>,
        depth: u32,
    ) -> Option<SyntaxNode<'t>> {
        if depth > 4 {
            return None;
        }
        let args = get_child_by_field(call_node, "arguments")?;
        for arg in named_children(args) {
            if arg.kind() == "arrow_function" || arg.kind() == "function_expression" {
                if let Some(obj) = self.function_returned_object(arg) {
                    return Some(obj);
                }
            } else if arg.kind() == "call_expression" {
                if let Some(obj) = self.find_initializer_returned_object(arg, depth + 1) {
                    return Some(obj);
                }
            }
        }
        None
    }

    /// The object literal a function expression returns — either the `=> ({...})`
    /// arrow form (a parenthesized_expression wrapping an object) or a
    /// `=> { return {...} }` block. Returns None for any other body shape.
    pub(super) fn function_returned_object<'t>(
        &self,
        fn_node: SyntaxNode<'t>,
    ) -> Option<SyntaxNode<'t>> {
        let body = get_child_by_field(fn_node, "body")?;

        // Recursion guard: `(((...)))` parenthesis towers nest as deep as the
        // source allows, and this descends them off the guarded `visit_node`
        // path (reached via `extract_variable` → `find_initializer_returned_object`),
        // so without a guard a pathological initializer overflows the worker
        // stack — the same failure class as the unguarded walkers above.
        fn as_object<'t>(n: SyntaxNode<'t>) -> Option<SyntaxNode<'t>> {
            ensure_sufficient_stack(|| {
                if n.kind() == "object" || n.kind() == "object_expression" {
                    return Some(n);
                }
                if n.kind() == "parenthesized_expression" {
                    for inner in named_children(n) {
                        if let Some(obj) = as_object(inner) {
                            return Some(obj);
                        }
                    }
                }
                None
            })
        }

        // `(set, get) => ({...})` — body is the (parenthesized) object directly.
        if let Some(direct) = as_object(body) {
            return Some(direct);
        }
        // `(set, get) => { return {...} }` — scan top-level return statements.
        if body.kind() == "statement_block" {
            for stmt in named_children(body) {
                if stmt.kind() != "return_statement" {
                    continue;
                }
                for child in named_children(stmt) {
                    if let Some(obj) = as_object(child) {
                        return Some(obj);
                    }
                }
            }
        }
        None
    }
}

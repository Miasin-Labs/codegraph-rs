//! Lua language extraction config.
//!
//! Ported from `src/extraction/languages/lua.ts`.
//!
//! Node names follow the ABI-15 grammar (`tree-sitter-lua` 0.5 /
//! @tree-sitter-grammars/tree-sitter-lua), NOT the older tree-sitter-wasms
//! build — see grammars.rs.

use super::named_children;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{
    ExtractorContext,
    LanguageExtractor,
    NodeExtra,
    SyntaxNode,
};
use crate::types::{EdgeKind, NodeKind, UnresolvedReference};

/// First descendant of a given type (breadth-first), or None.
fn find_descendant<'t>(node: SyntaxNode<'t>, kind: &str) -> Option<SyntaxNode<'t>> {
    let mut queue: std::collections::VecDeque<SyntaxNode<'t>> =
        named_children(node).into_iter().collect();
    while let Some(n) = queue.pop_front() {
        if n.kind() == kind {
            return Some(n);
        }
        queue.extend(named_children(n));
    }
    None
}

/// If `call_node` is a `require(...)` call, return the module name; otherwise None.
/// Lua/Luau have no import statement — modules are loaded by calling the global
/// `require`. Handles both:
///   - string requires:  `require("net.http")` / `require "net.http"`  → "net.http"
///   - Roblox/Luau path requires: `require(script.Parent.Signal)`      → "Signal"
///     (the dominant idiom in Roblox code, where the argument is an instance path
///     rather than a string — use the trailing field as the module name).
pub(super) fn require_module(call_node: SyntaxNode<'_>, source: &str) -> Option<String> {
    // function_call > name: <callee>, arguments: arguments
    let name = get_child_by_field(call_node, "name")?;
    // A dotted/colon callee (e.g. `socket.connect`) is dot/method_index_expression,
    // never a bare `require`.
    if name.kind() != "identifier" {
        return None;
    }
    if get_node_text(name, source) != "require" {
        return None;
    }

    let args = get_child_by_field(call_node, "arguments")?;

    // String require — `string > content: string_content` gives the bare name.
    if let Some(content) = find_descendant(args, "string_content") {
        let t = get_node_text(content, source).trim();
        return if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        };
    }
    if let Some(str_node) = find_descendant(args, "string") {
        // TS: trim, strip leading `[[`, trailing `]]`, leading quote, trailing quote
        let module = get_node_text(str_node, source).trim();
        let module = module.strip_prefix("[[").unwrap_or(module);
        let module = module.strip_suffix("]]").unwrap_or(module);
        let module = module
            .strip_prefix('"')
            .or_else(|| module.strip_prefix('\''))
            .unwrap_or(module);
        let module = module
            .strip_suffix('"')
            .or_else(|| module.strip_suffix('\''))
            .unwrap_or(module);
        if !module.is_empty() {
            return Some(module.to_string());
        }
    }

    // Roblox/Luau instance-path require: `require(script.Parent.Signal)` → "Signal".
    let idx = find_descendant(args, "dot_index_expression")
        .or_else(|| find_descendant(args, "method_index_expression"));
    if let Some(idx) = idx {
        let field = get_child_by_field(idx, "field").or_else(|| get_child_by_field(idx, "method"));
        if let Some(field) = field {
            let t = get_node_text(field, source).trim();
            return if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            };
        }
    }
    None
}

/// Emit an import node (+ imports reference) for a `require(...)` call.
fn emit_require(call_node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) {
    let Some(module) = require_module(call_node, ctx.source()) else {
        return;
    };
    let signature: String = get_node_text(call_node, ctx.source())
        .trim()
        .chars()
        .take(100)
        .collect();
    let imp = ctx.create_node(
        NodeKind::Import,
        &module,
        call_node,
        NodeExtra {
            signature: Some(signature),
            ..Default::default()
        },
    );
    if imp.is_some() {
        if let Some(parent_id) = ctx.node_stack().last().cloned() {
            ctx.add_unresolved_reference(UnresolvedReference {
                from_node_id: parent_id,
                reference_name: module,
                reference_kind: EdgeKind::Imports,
                line: call_node.start_position().row as u32 + 1,
                column: call_node.start_position().column as u32,
                file_path: None,
                language: None,
                candidates: None,
                metadata: None,
            });
        }
    }
}

pub struct LuaExtractor;

impl LanguageExtractor for LuaExtractor {
    fn function_types(&self) -> &[&str] {
        // function_declaration covers global (`function f`), table (`function t.f`),
        // method (`function t:m`), and local (`local function f`) forms — the form is
        // distinguished by the `name:` child (identifier / dot_index_expression /
        // method_index_expression) and a `local` token, not by separate node types.
        // Anonymous `function() ... end` (function_definition) has no name and is
        // captured via its enclosing variable instead.
        &["function_declaration"]
    }
    fn class_types(&self) -> &[&str] {
        // Lua has no classes/structs/interfaces/enums — tables are used for everything
        &[]
    }
    fn method_types(&self) -> &[&str] {
        &[]
    }
    fn interface_types(&self) -> &[&str] {
        &[]
    }
    fn struct_types(&self) -> &[&str] {
        &[]
    }
    fn enum_types(&self) -> &[&str] {
        &[]
    }
    fn type_alias_types(&self) -> &[&str] {
        &[]
    }
    fn import_types(&self) -> &[&str] {
        // `require` is a function_call — handled in visit_node below
        &[]
    }
    fn call_types(&self) -> &[&str] {
        &["function_call"]
    }
    fn variable_types(&self) -> &[&str] {
        // see the `lua` branch in extractVariable
        &["variable_declaration"]
    }
    fn name_field(&self) -> &str {
        "name"
    }
    fn body_field(&self) -> &str {
        "body"
    }
    fn params_field(&self) -> &str {
        "parameters"
    }

    fn get_signature(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        let params = get_child_by_field(node, "parameters")?;
        Some(get_node_text(params, source).to_string())
    }

    /// `function t.f()` / `function t:m()` are methods on table `t`: return the
    /// table as the receiver so they extract as methods with a `t::f` qualified
    /// name. Plain `function f()` / `local function f()` have no receiver and stay
    /// functions. (For `a.b.c`, the receiver is the nested `a.b`.)
    fn get_receiver_type(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        let name = get_child_by_field(node, "name")?;
        if name.kind() == "dot_index_expression" || name.kind() == "method_index_expression" {
            if let Some(table) = get_child_by_field(name, "table") {
                return Some(get_node_text(table, source).to_string());
            }
        }
        None
    }

    /// Emit import nodes for `require(...)`. The local-declaration form is handled
    /// explicitly because the variable branch skips the initializer subtree; bare
    /// and global `require` calls are caught when the walker reaches the
    /// function_call node.
    fn visit_node(&self, node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
        // Bare / global `require("x")` — claim it so it isn't double-counted as a call.
        if node.kind() == "function_call" {
            if require_module(node, ctx.source()).is_some() {
                emit_require(node, ctx);
                return true;
            }
            return false;
        }

        // `local x = require("x")` — variable_declaration wraps an assignment_statement
        // whose initializer subtree the variable branch will skip, so dig it out here.
        if node.kind() == "variable_declaration" {
            let assign = named_children(node)
                .into_iter()
                .find(|c| c.kind() == "assignment_statement");
            let expr_list = assign.and_then(|a| {
                named_children(a)
                    .into_iter()
                    .find(|c| c.kind() == "expression_list")
            });
            if let Some(expr_list) = expr_list {
                for val in named_children(expr_list) {
                    if val.kind() == "function_call" {
                        emit_require(val, ctx);
                    }
                }
            }
            return false;
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
    use crate::types::{Language, NodeKind};

    #[test]
    fn lua_smoke_extraction() {
        let source = "local http = require(\"net.http\")\n\nlocal M = {}\n\nfunction M.fetch(url)\n  return http.get(url)\nend\n\nfunction helper()\nend\n\nreturn M\n";
        let result = TreeSitterExtractor::new(
            "src/client.lua",
            source,
            Some(Language::Lua),
            Some(&LuaExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        // require via visit_node hook
        let import = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Import)
            .expect("import node");
        assert_eq!(import.name, "net.http");

        // table function → method with receiver
        let fetch = result.nodes.iter().find(|n| n.name == "fetch").unwrap();
        assert!(
            fetch.qualified_name.contains('M'),
            "table function should carry receiver, got {:?}",
            fetch.qualified_name
        );

        let helper = result.nodes.iter().find(|n| n.name == "helper").unwrap();
        assert_eq!(helper.kind, NodeKind::Function);
    }

    #[test]
    fn lua_require_module_forms() {
        let cases = [
            ("local a = require(\"net.http\")", Some("net.http")),
            ("local a = require 'json'", Some("json")),
            ("local sig = require(script.Parent.Signal)", Some("Signal")),
            ("local x = compute(1)", None),
        ];
        for (src, expected) in cases {
            let mut parser = crate::extraction::grammars::create_parser(Language::Lua).unwrap();
            let tree = parser.parse(src, None).unwrap();
            let call = find_descendant(tree.root_node(), "function_call");
            let got = call.and_then(|c| require_module(c, src));
            assert_eq!(got.as_deref(), expected, "for source {src:?}");
        }
    }
}

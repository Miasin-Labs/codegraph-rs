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

    /// Emit import nodes for `require(...)` and function nodes for function
    /// expressions bound to table fields / table-constructor keys.
    ///
    /// The local-declaration form of `require` is handled explicitly because the
    /// variable branch skips the initializer subtree; bare and global `require`
    /// calls are caught when the walker reaches the function_call node.
    ///
    /// Function expressions (`function_definition`) assigned to a name have no
    /// `function_declaration` wrapper, so the generic walker never treats them as
    /// functions — their body calls would wrongly attribute to the file node
    /// (upstream #1616). We claim the enclosing `assignment_statement` and emit a
    /// function/method node per bound function expression, then walk each body
    /// under the new node so its calls attribute correctly.
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

        // `M.assignedFn = function() ... end` / `g = function() ... end` /
        // `M.callbacks = { onStart = function() ... end }` — function expressions
        // bound to a target. `function_declaration` forms (`function M.f()`,
        // `local function f()`) are NOT assignment_statements, so this never
        // double-extracts them.
        if node.kind() == "assignment_statement" {
            return handle_function_assignment(node, ctx);
        }

        false
    }
}

/// Name + optional receiver for an assignment target.
///
/// - `identifier` (`g = ...`)            → (`g`, None)          → Function
/// - `dot_index_expression` (`M.f = ...`) → (`f`, Some(`M`))     → Method `M::f`
///
/// Mirrors `get_receiver_type` / `extract_name` for the `function t.f()` form so
/// `M.assignedFn = function() end` extracts identically to `function M.assignedFn()`.
/// (`method_index_expression` cannot appear on the left of `=` in Lua, but is
/// handled defensively.)
fn target_name_and_receiver(target: SyntaxNode<'_>, source: &str) -> (String, Option<String>) {
    match target.kind() {
        "identifier" => (get_node_text(target, source).to_string(), None),
        "dot_index_expression" => {
            let name = get_child_by_field(target, "field")
                .map(|f| get_node_text(f, source).to_string())
                .unwrap_or_default();
            let receiver =
                get_child_by_field(target, "table").map(|t| get_node_text(t, source).to_string());
            (name, receiver)
        }
        "method_index_expression" => {
            let name = get_child_by_field(target, "method")
                .map(|m| get_node_text(m, source).to_string())
                .unwrap_or_default();
            let receiver =
                get_child_by_field(target, "table").map(|t| get_node_text(t, source).to_string());
            (name, receiver)
        }
        _ => (String::new(), None),
    }
}

/// Emit a function/method node for a `function_definition` bound to `name`, then
/// walk its body so its calls/instantiations attribute to the new node instead of
/// the enclosing file. A receiver makes it a method with a `receiver::name`
/// qualified name (parity with `function t.f()`).
fn emit_function_binding(
    func_def: SyntaxNode<'_>,
    name: &str,
    receiver: Option<&str>,
    ctx: &mut dyn ExtractorContext,
) {
    if name.is_empty() {
        // Anonymous target we cannot name — still walk the body so its calls are
        // not lost (they stay attributed to the enclosing scope, as before).
        if let Some(body) = get_child_by_field(func_def, "body") {
            ctx.visit_function_body(body, "");
        }
        return;
    }
    let signature = get_child_by_field(func_def, "parameters")
        .map(|p| get_node_text(p, ctx.source()).to_string());
    let (kind, qualified_name) = match receiver {
        Some(r) => (NodeKind::Method, Some(format!("{}::{}", r, name))),
        None => (NodeKind::Function, None),
    };
    let created = ctx.create_node(
        kind,
        name,
        func_def,
        NodeExtra {
            signature,
            qualified_name,
            ..Default::default()
        },
    );
    if let Some(created) = created {
        ctx.push_scope(created.id.clone());
        if let Some(body) = get_child_by_field(func_def, "body") {
            ctx.visit_function_body(body, &created.id);
        }
        ctx.pop_scope();
    }
}

/// Emit function nodes for `field = function() ... end` entries of a
/// `table_constructor`, recursing into nested table literals. Non-function field
/// values are dispatched through the normal walker so their calls/requires are
/// still recorded.
fn emit_table_function_fields(table: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) {
    for field in named_children(table) {
        if field.kind() != "field" {
            // Positional / non-`field` entries (arrays, bare expressions) — walk
            // normally so any calls inside are recorded.
            ctx.visit_node(field);
            continue;
        }
        let Some(value) = get_child_by_field(field, "value") else {
            continue;
        };
        match value.kind() {
            "function_definition" => {
                // Only identifier keys yield a name; bracket/string keys fall
                // through to a normal body walk (calls preserved, no named node).
                let key = get_child_by_field(field, "name")
                    .filter(|k| k.kind() == "identifier")
                    .map(|k| get_node_text(k, ctx.source()).to_string())
                    .unwrap_or_default();
                emit_function_binding(value, &key, None, ctx);
            }
            "table_constructor" => emit_table_function_fields(value, ctx),
            _ => ctx.visit_node(value),
        }
    }
}

/// Handle an `assignment_statement` whose right-hand side binds a function
/// expression (directly, or inside a table-constructor key). Returns `true` when
/// it claimed the node (so the generic walker skips its children); `false` leaves
/// the assignment to the default traversal (e.g. `x = require("y")`, `x = 5`).
fn handle_function_assignment(node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
    let children = named_children(node);
    let Some(var_list) = children
        .iter()
        .find(|c| c.kind() == "variable_list")
        .copied()
    else {
        return false;
    };
    let Some(expr_list) = children
        .iter()
        .find(|c| c.kind() == "expression_list")
        .copied()
    else {
        return false;
    };
    let targets = named_children(var_list);
    let values = named_children(expr_list);

    // Only claim assignments that actually bind a function expression somewhere;
    // otherwise let the default walker handle them (require calls, plain data).
    let binds_function = values
        .iter()
        .any(|v| matches!(v.kind(), "function_definition" | "table_constructor"));
    if !binds_function {
        return false;
    }

    for (i, value) in values.iter().enumerate() {
        let target = targets.get(i).copied();
        match value.kind() {
            "function_definition" => match target {
                Some(target) => {
                    let (name, receiver) = target_name_and_receiver(target, ctx.source());
                    emit_function_binding(*value, &name, receiver.as_deref(), ctx);
                }
                None => emit_function_binding(*value, "", None, ctx),
            },
            "table_constructor" => emit_table_function_fields(*value, ctx),
            // Other values (calls, requires, ...) still need normal handling.
            _ => ctx.visit_node(*value),
        }
    }

    // Index targets like `t[compute()] = ...` may carry calls — walk non-identifier
    // targets so those are not dropped by claiming the assignment.
    for target in &targets {
        if target.kind() != "identifier" {
            ctx.visit_node(*target);
        }
    }

    true
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

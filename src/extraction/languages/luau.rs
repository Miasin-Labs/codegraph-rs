//! Luau language extraction config.
//!
//! Ported from `src/extraction/languages/luau.ts`.
//!
//! Luau (https://luau.org) is a gradually-typed superset of Lua. The
//! tree-sitter-luau grammar reuses the same node names as the Lua grammar
//! (function_declaration, variable_declaration, function_call,
//! dot/method_index_expression, …), so the Luau extractor extends the Lua one
//! and adds the type-system pieces Luau introduces:
//!   - `type X = ...` / `export type X = ...`  → type_definition (type_alias)
//!   - typed parameters and return types        → richer signatures
//!
//! require detection, receiver-splitting (t.f / t:m → methods), and local
//! variable extraction are inherited unchanged from LuaExtractor (the TS
//! `...luaExtractor` spread becomes explicit delegation). The shared
//! `extractVariable` core branch is gated on `lua` || `luau`.

use super::lua::LuaExtractor;
use super::named_children;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{ExtractorContext, LanguageExtractor, SyntaxNode};

/// Delegation target for the inherited (spread) Lua hooks.
static LUA: LuaExtractor = LuaExtractor;

pub struct LuauExtractor;

impl LanguageExtractor for LuauExtractor {
    // --- inherited from luaExtractor (TS object spread) ---
    fn function_types(&self) -> &[&str] {
        LUA.function_types()
    }
    fn class_types(&self) -> &[&str] {
        LUA.class_types()
    }
    fn method_types(&self) -> &[&str] {
        LUA.method_types()
    }
    fn interface_types(&self) -> &[&str] {
        LUA.interface_types()
    }
    fn struct_types(&self) -> &[&str] {
        LUA.struct_types()
    }
    fn enum_types(&self) -> &[&str] {
        LUA.enum_types()
    }
    fn import_types(&self) -> &[&str] {
        LUA.import_types()
    }
    fn call_types(&self) -> &[&str] {
        LUA.call_types()
    }
    fn variable_types(&self) -> &[&str] {
        LUA.variable_types()
    }
    fn name_field(&self) -> &str {
        LUA.name_field()
    }
    fn body_field(&self) -> &str {
        LUA.body_field()
    }
    fn params_field(&self) -> &str {
        LUA.params_field()
    }
    fn get_receiver_type(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        LUA.get_receiver_type(node, source)
    }
    fn visit_node(&self, node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
        LUA.visit_node(node, ctx)
    }

    // --- Luau additions/overrides ---

    /// `type X = ...` and `export type X = ...`
    fn type_alias_types(&self) -> &[&str] {
        &["type_definition"]
    }

    /// Only Luau `export type` is exported; the keyword leads the node.
    fn is_exported(&self, node: SyntaxNode<'_>, source: &str) -> Option<bool> {
        Some(source.get(node.start_byte()..node.start_byte() + 7) == Some("export "))
    }

    /// Params + Luau return type (the named child after `parameters`, before the body).
    fn get_signature(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        let params = get_child_by_field(node, "parameters")?;
        let mut sig = get_node_text(params, source).to_string();
        let kids = named_children(node);
        let idx = kids
            .iter()
            .position(|c| c.start_byte() == params.start_byte());
        let ret = idx.and_then(|i| kids.get(i + 1));
        if let Some(ret) = ret {
            if ret.kind() != "block" {
                sig.push_str(": ");
                sig.push_str(get_node_text(*ret, source));
            }
        }
        Some(sig)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
    use crate::types::{Language, NodeKind};

    #[test]
    fn luau_smoke_extraction() {
        let source = "local Signal = require(script.Parent.Signal)\n\nexport type Point = { x: number, y: number }\ntype Internal = { id: number }\n\nfunction make(x: number): number\n  return x\nend\n";
        let result = TreeSitterExtractor::new(
            "src/point.luau",
            source,
            Some(Language::Luau),
            Some(&LuauExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        // Roblox-style instance-path require inherited from Lua
        let import = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Import)
            .expect("import node");
        assert_eq!(import.name, "Signal");

        let point = result
            .nodes
            .iter()
            .find(|n| n.name == "Point")
            .expect("Point type alias");
        assert_eq!(point.kind, NodeKind::TypeAlias);
        assert_eq!(point.is_exported, Some(true));

        let internal = result
            .nodes
            .iter()
            .find(|n| n.name == "Internal")
            .expect("Internal type alias");
        assert_eq!(internal.is_exported, Some(false));

        let make = result.nodes.iter().find(|n| n.name == "make").unwrap();
        assert_eq!(make.kind, NodeKind::Function);
        assert_eq!(make.signature.as_deref(), Some("(x: number): number"));
    }
}

//! Ruby language extraction config.
//!
//! Ported from `src/extraction/languages/ruby.ts`.

use super::find_named_child;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{
    ExtractorContext,
    ImportInfo,
    ImportOutcome,
    LanguageExtractor,
    NodeExtra,
    SyntaxNode,
};
use crate::types::{NodeKind, Visibility};

/// Only statement-level identifiers — direct children of block/body nodes.
const BLOCK_PARENTS: [&str; 8] = [
    "body_statement",
    "then",
    "else",
    "do",
    "begin",
    "rescue",
    "ensure",
    "when",
];

/// Ruby keywords/literals to skip for bare-call detection.
const SKIP: [&str; 8] = [
    "true", "false", "nil", "self", "super", "__FILE__", "__LINE__", "__dir__",
];

pub struct RubyExtractor;

impl LanguageExtractor for RubyExtractor {
    fn function_types(&self) -> &[&str] {
        &["method"]
    }
    fn class_types(&self) -> &[&str] {
        &["class"]
    }
    fn method_types(&self) -> &[&str] {
        &["method", "singleton_method"]
    }
    fn interface_types(&self) -> &[&str] {
        // Ruby uses modules (handled via visit_node hook)
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
        // require/require_relative
        &["call"]
    }
    fn call_types(&self) -> &[&str] {
        &["call", "method_call"]
    }
    fn variable_types(&self) -> &[&str] {
        // Ruby uses assignment like Python
        &["assignment"]
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

    fn visit_node(&self, node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
        if node.kind() != "module" {
            return false;
        }

        let Some(name_node) = node.child_by_field_name("name") else {
            return false;
        };
        let name = get_node_text(name_node, ctx.source()).to_string();

        let Some(module_node) =
            ctx.create_node(NodeKind::Module, &name, node, NodeExtra::default())
        else {
            return false;
        };

        // Push module onto scope stack so children get proper qualified names
        ctx.push_scope(module_node.id);
        if let Some(body) = node.child_by_field_name("body") {
            for i in 0..body.named_child_count() as u32 {
                if let Some(child) = body.named_child(i) {
                    ctx.visit_node(child);
                }
            }
        }
        ctx.pop_scope();
        true // handled
    }

    fn extract_bare_call(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        // Ruby bare method calls (no parens, no receiver) parse as plain identifiers.
        // e.g., `reset` in a method body is `identifier "reset"` not a `call` node.
        if node.kind() != "identifier" {
            return None;
        }

        let parent = node.parent()?;

        if !BLOCK_PARENTS.contains(&parent.kind()) {
            return None;
        }

        let name = get_node_text(node, source);

        // Skip Ruby keywords/literals
        if SKIP.contains(&name) {
            return None;
        }

        // Skip constants (uppercase start) — these are class/module refs, not calls
        if name
            .as_bytes()
            .first()
            .map(|b| b.is_ascii_uppercase())
            .unwrap_or(false)
        {
            return None;
        }

        Some(name.to_string())
    }

    fn get_visibility(&self, node: SyntaxNode<'_>, source: &str) -> Option<Visibility> {
        // Ruby visibility is based on preceding visibility modifiers
        let mut sibling = node.prev_named_sibling();
        while let Some(sib) = sibling {
            if sib.kind() == "call" {
                if let Some(method_name) = get_child_by_field(sib, "method") {
                    match get_node_text(method_name, source) {
                        "private" => return Some(Visibility::Private),
                        "protected" => return Some(Visibility::Protected),
                        "public" => return Some(Visibility::Public),
                        _ => {}
                    }
                }
            }
            sibling = sib.prev_named_sibling();
        }
        Some(Visibility::Public)
    }

    fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
        let import_text = get_node_text(node, source).trim();

        // Check if this is a require/require_relative call
        let Some(identifier) = find_named_child(node, "identifier") else {
            return ImportOutcome::Declined;
        };
        let method_name = get_node_text(identifier, source);
        if method_name != "require" && method_name != "require_relative" {
            // Not an import, skip
            return ImportOutcome::Declined;
        }

        // Find the argument (string)
        if let Some(arg_list) = find_named_child(node, "argument_list") {
            if let Some(string_node) = find_named_child(arg_list, "string") {
                if let Some(string_content) = find_named_child(string_node, "string_content") {
                    return ImportOutcome::Info(ImportInfo::new(
                        get_node_text(string_content, source),
                        import_text,
                    ));
                }
            }
        }
        ImportOutcome::Declined
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
    use crate::types::{EdgeKind, Language, NodeKind};

    #[test]
    fn ruby_smoke_extraction() {
        let source = "require 'json'\n\nmodule Billing\n  class Invoice\n    def total\n      compute\n    end\n\n    private\n\n    def compute\n      42\n    end\n  end\nend\n";
        let result = TreeSitterExtractor::new(
            "lib/invoice.rb",
            source,
            Some(Language::Ruby),
            Some(&RubyExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        // module handled via visit_node hook
        let module = result.nodes.iter().find(|n| n.name == "Billing").unwrap();
        assert_eq!(module.kind, NodeKind::Module);

        let class = result.nodes.iter().find(|n| n.name == "Invoice").unwrap();
        assert_eq!(class.kind, NodeKind::Class);
        assert!(
            class.qualified_name.contains("Billing"),
            "class should be scoped under module, got {:?}",
            class.qualified_name
        );

        let total = result.nodes.iter().find(|n| n.name == "total").unwrap();
        assert_eq!(total.kind, NodeKind::Method);
        assert_eq!(total.visibility, Some(Visibility::Public));

        let compute = result.nodes.iter().find(|n| n.name == "compute").unwrap();
        // TS-parity: the hook only recognizes `call`-typed siblings (e.g.
        // `private :foo`); a bare `private` line parses as a plain identifier
        // in tree-sitter-ruby, so it is not seen — identical to TS behavior
        // (the TS suite does not assert Ruby visibility).
        assert_eq!(compute.visibility, Some(Visibility::Public));

        // bare call `compute` inside `total` recorded via extract_bare_call
        let bare_call = result
            .unresolved_references
            .iter()
            .find(|r| r.reference_name == "compute" && r.reference_kind == EdgeKind::Calls)
            .expect("bare call reference");
        assert_eq!(bare_call.from_node_id, total.id);

        let import = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Import)
            .expect("import node");
        assert_eq!(import.name, "json");
    }
}

//! Rust language extraction config.
//!
//! Ported from `src/extraction/languages/rust.ts`.

use super::named_children;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{
    ImportInfo,
    ImportOutcome,
    LanguageExtractor,
    SyntaxNode,
};
use crate::types::{NodeKind, Visibility};

/// Helper to get the root crate/module from a scoped path.
fn get_root_module(scoped_node: SyntaxNode<'_>, source: &str) -> String {
    // Recursion guard — depth driven by nested scoped_identifier path segments.
    crate::ensure_sufficient_stack(|| get_root_module_inner(scoped_node, source))
}

fn get_root_module_inner(scoped_node: SyntaxNode<'_>, source: &str) -> String {
    let Some(first_child) = scoped_node.named_child(0) else {
        return get_node_text(scoped_node, source).to_string();
    };
    match first_child.kind() {
        "identifier" | "crate" | "super" | "self" => get_node_text(first_child, source).to_string(),
        "scoped_identifier" => get_root_module(first_child, source),
        _ => get_node_text(first_child, source).to_string(),
    }
}

pub struct RustExtractor;

impl LanguageExtractor for RustExtractor {
    fn function_types(&self) -> &[&str] {
        &["function_item"]
    }
    fn class_types(&self) -> &[&str] {
        // Rust has impl blocks
        &[]
    }
    fn method_types(&self) -> &[&str] {
        // Methods are functions in impl blocks
        &["function_item"]
    }
    fn interface_types(&self) -> &[&str] {
        &["trait_item"]
    }
    fn struct_types(&self) -> &[&str] {
        &["struct_item"]
    }
    fn enum_types(&self) -> &[&str] {
        &["enum_item"]
    }
    fn enum_member_types(&self) -> &[&str] {
        &["enum_variant"]
    }
    fn type_alias_types(&self) -> &[&str] {
        // Rust type aliases
        &["type_item"]
    }
    fn import_types(&self) -> &[&str] {
        &["use_declaration"]
    }
    fn call_types(&self) -> &[&str] {
        &["call_expression"]
    }
    fn variable_types(&self) -> &[&str] {
        &["let_declaration", "const_item", "static_item"]
    }
    fn interface_kind(&self) -> NodeKind {
        NodeKind::Trait
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
    fn return_field(&self) -> Option<&str> {
        Some("return_type")
    }

    fn get_signature(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        let params = get_child_by_field(node, "parameters")?;
        let return_type = get_child_by_field(node, "return_type");
        let mut sig = get_node_text(params, source).to_string();
        if let Some(rt) = return_type {
            sig.push_str(" -> ");
            sig.push_str(get_node_text(rt, source));
        }
        Some(sig)
    }

    fn is_async(&self, node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
        for i in 0..node.child_count() as u32 {
            if let Some(child) = node.child(i) {
                if child.kind() == "async" {
                    return Some(true);
                }
            }
        }
        Some(false)
    }

    fn get_visibility(&self, node: SyntaxNode<'_>, source: &str) -> Option<Visibility> {
        for i in 0..node.child_count() as u32 {
            if let Some(child) = node.child(i) {
                if child.kind() == "visibility_modifier" {
                    return Some(if get_node_text(child, source).contains("pub") {
                        Visibility::Public
                    } else {
                        Visibility::Private
                    });
                }
            }
        }
        // Rust defaults to private
        Some(Visibility::Private)
    }

    fn get_receiver_type(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        // Walk up the tree-sitter AST to find a parent impl_item
        let mut parent = node.parent();
        while let Some(p) = parent {
            if p.kind() == "impl_item" {
                // For `impl Type { ... }` — the type is a direct type_identifier child
                // For `impl Trait for Type { ... }` — the type is the LAST type_identifier
                // (the first is part of the trait path)
                let children = named_children(p);
                // Find all direct type_identifier children (not nested in scoped paths)
                let type_idents: Vec<_> = children
                    .iter()
                    .filter(|c| c.kind() == "type_identifier")
                    .collect();
                if let Some(type_node) = type_idents.last() {
                    // Last type_identifier is always the implementing type
                    return Some(get_node_text(**type_node, source).to_string());
                }
                // Handle generic types: impl<T> MyStruct<T> { ... }
                if let Some(generic_type) = children.iter().find(|c| c.kind() == "generic_type") {
                    if let Some(inner_type) = named_children(*generic_type)
                        .into_iter()
                        .find(|c| c.kind() == "type_identifier")
                    {
                        return Some(get_node_text(inner_type, source).to_string());
                    }
                }
                return None;
            }
            parent = p.parent();
        }
        None
    }

    fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
        let import_text = get_node_text(node, source).trim();

        // Find the use argument (scoped_use_list or scoped_identifier)
        let use_arg = named_children(node).into_iter().find(|c| {
            matches!(
                c.kind(),
                "scoped_use_list" | "scoped_identifier" | "use_list" | "identifier"
            )
        });

        if let Some(use_arg) = use_arg {
            return ImportOutcome::Info(ImportInfo::new(
                get_root_module(use_arg, source),
                import_text,
            ));
        }
        ImportOutcome::Declined
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
    use crate::types::{Language, NodeKind};

    #[test]
    fn rust_smoke_extraction() {
        let source = "use std::collections::HashMap;\n\npub struct Config {\n    pub name: String,\n}\n\npub trait Runner {\n    fn run(&self);\n}\n\npub enum Mode {\n    Fast,\n    Slow,\n}\n\nimpl Config {\n    pub fn load(path: &str) -> Config {\n        helper();\n        Config { name: path.to_string() }\n    }\n}\n\nasync fn helper() {}\n";
        let result = TreeSitterExtractor::new(
            "src/lib.rs",
            source,
            Some(Language::Rust),
            Some(&RustExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let config = result.nodes.iter().find(|n| n.name == "Config").unwrap();
        assert_eq!(config.kind, NodeKind::Struct);
        assert_eq!(config.visibility, Some(Visibility::Public));

        let runner = result.nodes.iter().find(|n| n.name == "Runner").unwrap();
        // interface_kind override → trait
        assert_eq!(runner.kind, NodeKind::Trait);

        let mode = result.nodes.iter().find(|n| n.name == "Mode").unwrap();
        assert_eq!(mode.kind, NodeKind::Enum);
        let fast = result.nodes.iter().find(|n| n.name == "Fast").unwrap();
        assert_eq!(fast.kind, NodeKind::EnumMember);

        let load = result.nodes.iter().find(|n| n.name == "load").unwrap();
        assert!(
            load.qualified_name.contains("Config"),
            "impl method should carry receiver type, got {:?}",
            load.qualified_name
        );
        assert_eq!(load.signature.as_deref(), Some("(path: &str) -> Config"));

        let helper = result.nodes.iter().find(|n| n.name == "helper").unwrap();
        // TS-parity: the hook scans direct children for an `async` token, but
        // tree-sitter-rust nests it under `function_modifiers`, so detection
        // misses — identical to the TS behavior on the same grammar shape
        // (the TS suite does not assert Rust isAsync).
        assert_eq!(helper.is_async, Some(false));
        assert_eq!(helper.visibility, Some(Visibility::Private));

        let import = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Import)
            .expect("import node");
        // Root module of `use std::collections::HashMap;` is `std`
        assert_eq!(import.name, "std");
    }
}

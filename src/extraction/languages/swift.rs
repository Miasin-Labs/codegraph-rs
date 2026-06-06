//! Swift language extraction config.
//!
//! Ported from `src/extraction/languages/swift.ts`.

use super::find_named_child;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{
    ClassLikeKind,
    ImportInfo,
    ImportOutcome,
    LanguageExtractor,
    SyntaxNode,
};
use crate::types::Visibility;

pub struct SwiftExtractor;

impl LanguageExtractor for SwiftExtractor {
    fn function_types(&self) -> &[&str] {
        &["function_declaration"]
    }
    fn class_types(&self) -> &[&str] {
        &["class_declaration"]
    }
    fn method_types(&self) -> &[&str] {
        // Methods are functions inside classes
        &["function_declaration"]
    }
    fn interface_types(&self) -> &[&str] {
        &["protocol_declaration"]
    }
    fn struct_types(&self) -> &[&str] {
        &["struct_declaration"]
    }
    fn enum_types(&self) -> &[&str] {
        &["enum_declaration"]
    }
    fn enum_member_types(&self) -> &[&str] {
        &["enum_entry"]
    }
    fn type_alias_types(&self) -> &[&str] {
        &["typealias_declaration"]
    }
    fn import_types(&self) -> &[&str] {
        &["import_declaration"]
    }
    fn call_types(&self) -> &[&str] {
        &["call_expression"]
    }
    fn variable_types(&self) -> &[&str] {
        &["property_declaration", "constant_declaration"]
    }
    fn name_field(&self) -> &str {
        "name"
    }
    fn body_field(&self) -> &str {
        "body"
    }
    fn params_field(&self) -> &str {
        "parameter"
    }
    fn return_field(&self) -> Option<&str> {
        Some("return_type")
    }

    fn get_signature(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        // Swift function signature: func name(params) -> ReturnType
        let params = get_child_by_field(node, "parameter")?;
        let return_type = get_child_by_field(node, "return_type");
        let mut sig = get_node_text(params, source).to_string();
        if let Some(rt) = return_type {
            sig.push_str(" -> ");
            sig.push_str(get_node_text(rt, source));
        }
        Some(sig)
    }

    fn get_visibility(&self, node: SyntaxNode<'_>, source: &str) -> Option<Visibility> {
        // Check for visibility modifiers in Swift
        for i in 0..node.child_count() as u32 {
            if let Some(child) = node.child(i) {
                if child.kind() == "modifiers" {
                    let text = get_node_text(child, source);
                    if text.contains("public") {
                        return Some(Visibility::Public);
                    }
                    if text.contains("private") {
                        return Some(Visibility::Private);
                    }
                    if text.contains("internal") {
                        return Some(Visibility::Internal);
                    }
                    if text.contains("fileprivate") {
                        return Some(Visibility::Private);
                    }
                }
            }
        }
        // Swift defaults to internal
        Some(Visibility::Internal)
    }

    fn is_static(&self, node: SyntaxNode<'_>, source: &str) -> Option<bool> {
        for i in 0..node.child_count() as u32 {
            if let Some(child) = node.child(i) {
                if child.kind() == "modifiers" {
                    let text = get_node_text(child, source);
                    if text.contains("static") || text.contains("class") {
                        return Some(true);
                    }
                }
            }
        }
        Some(false)
    }

    fn classify_class_node(&self, node: SyntaxNode<'_>, _source: &str) -> ClassLikeKind {
        // Swift uses class_declaration for classes, structs, and enums
        for i in 0..node.child_count() as u32 {
            if let Some(child) = node.child(i) {
                if child.kind() == "struct" {
                    return ClassLikeKind::Struct;
                }
                if child.kind() == "enum" {
                    return ClassLikeKind::Enum;
                }
            }
        }
        ClassLikeKind::Class
    }

    fn is_async(&self, node: SyntaxNode<'_>, source: &str) -> Option<bool> {
        for i in 0..node.child_count() as u32 {
            if let Some(child) = node.child(i) {
                if child.kind() == "modifiers" && get_node_text(child, source).contains("async") {
                    return Some(true);
                }
            }
        }
        Some(false)
    }

    fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
        let import_text = get_node_text(node, source).trim();
        if let Some(identifier) = find_named_child(node, "identifier") {
            return ImportOutcome::Info(ImportInfo::new(
                get_node_text(identifier, source),
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
    fn swift_smoke_extraction() {
        let source = "import Foundation\n\nclass Session {\n    func start() {\n        prepare()\n    }\n}\n\nstruct Point {\n    var x: Int\n}\n\nenum Direction {\n    case north\n    case south\n}\n\nprotocol Runnable {\n    func run()\n}\n\nfunc prepare() {}\n";
        let result = TreeSitterExtractor::new(
            "Sources/Session.swift",
            source,
            Some(Language::Swift),
            Some(&SwiftExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let class = result.nodes.iter().find(|n| n.name == "Session").unwrap();
        assert_eq!(class.kind, NodeKind::Class);

        // tree-sitter-swift reuses class_declaration for struct/enum — classified
        // via classify_class_node keyword scan.
        let point = result.nodes.iter().find(|n| n.name == "Point").unwrap();
        assert_eq!(point.kind, NodeKind::Struct);
        let direction = result.nodes.iter().find(|n| n.name == "Direction").unwrap();
        assert_eq!(direction.kind, NodeKind::Enum);

        let proto = result.nodes.iter().find(|n| n.name == "Runnable").unwrap();
        assert_eq!(proto.kind, NodeKind::Interface);

        let start = result.nodes.iter().find(|n| n.name == "start").unwrap();
        assert_eq!(start.kind, NodeKind::Method);
        assert_eq!(start.qualified_name, "Session::start");

        let func = result.nodes.iter().find(|n| n.name == "prepare").unwrap();
        assert_eq!(func.kind, NodeKind::Function);

        let import = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Import)
            .expect("import node");
        assert_eq!(import.name, "Foundation");
    }
}

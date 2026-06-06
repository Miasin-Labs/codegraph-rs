//! Java language extraction config.
//!
//! Ported from `src/extraction/languages/java.ts`.

use super::named_children;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{
    ImportInfo,
    ImportOutcome,
    LanguageExtractor,
    SyntaxNode,
};
use crate::types::Visibility;

pub struct JavaExtractor;

impl LanguageExtractor for JavaExtractor {
    fn function_types(&self) -> &[&str] {
        &[]
    }
    fn class_types(&self) -> &[&str] {
        &["class_declaration"]
    }
    fn method_types(&self) -> &[&str] {
        &["method_declaration", "constructor_declaration"]
    }
    fn interface_types(&self) -> &[&str] {
        &["interface_declaration"]
    }
    fn struct_types(&self) -> &[&str] {
        &[]
    }
    fn enum_types(&self) -> &[&str] {
        &["enum_declaration"]
    }
    fn enum_member_types(&self) -> &[&str] {
        &["enum_constant"]
    }
    fn type_alias_types(&self) -> &[&str] {
        &[]
    }
    fn import_types(&self) -> &[&str] {
        &["import_declaration"]
    }
    fn call_types(&self) -> &[&str] {
        &["method_invocation"]
    }
    fn variable_types(&self) -> &[&str] {
        &["local_variable_declaration"]
    }
    fn field_types(&self) -> &[&str] {
        &["field_declaration"]
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
        Some("type")
    }

    fn get_signature(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        let params = get_child_by_field(node, "parameters")?;
        let return_type = get_child_by_field(node, "type");
        let params_text = get_node_text(params, source);
        Some(match return_type {
            Some(rt) => format!("{} {}", get_node_text(rt, source), params_text),
            None => params_text.to_string(),
        })
    }

    fn get_visibility(&self, node: SyntaxNode<'_>, source: &str) -> Option<Visibility> {
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
                    if text.contains("protected") {
                        return Some(Visibility::Protected);
                    }
                }
            }
        }
        None
    }

    fn is_static(&self, node: SyntaxNode<'_>, source: &str) -> Option<bool> {
        for i in 0..node.child_count() as u32 {
            if let Some(child) = node.child(i) {
                if child.kind() == "modifiers" && get_node_text(child, source).contains("static") {
                    return Some(true);
                }
            }
        }
        Some(false)
    }

    fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
        let import_text = get_node_text(node, source).trim();
        if let Some(scoped_id) = named_children(node)
            .into_iter()
            .find(|c| c.kind() == "scoped_identifier")
        {
            return ImportOutcome::Info(ImportInfo::new(
                get_node_text(scoped_id, source),
                import_text,
            ));
        }
        ImportOutcome::Declined
    }

    fn package_types(&self) -> &[&str] {
        &["package_declaration"]
    }

    fn extract_package(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        // package_declaration → scoped_identifier or identifier (single-segment)
        named_children(node)
            .into_iter()
            .find(|c| c.kind() == "scoped_identifier" || c.kind() == "identifier")
            .map(|id| get_node_text(id, source).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
    use crate::types::{Language, NodeKind};

    #[test]
    fn java_smoke_extraction() {
        let source = "package com.example.app;\n\nimport java.util.List;\n\npublic class Account {\n    private static int count;\n\n    public void deposit(int amount) {\n        validate(amount);\n    }\n}\n\ninterface Validator {\n    boolean validate(int x);\n}\n\nenum Status { OPEN, CLOSED }\n";
        let result = TreeSitterExtractor::new(
            "src/Account.java",
            source,
            Some(Language::Java),
            Some(&JavaExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let class = result.nodes.iter().find(|n| n.name == "Account").unwrap();
        assert_eq!(class.kind, NodeKind::Class);
        assert_eq!(class.visibility, Some(Visibility::Public));

        let method = result.nodes.iter().find(|n| n.name == "deposit").unwrap();
        assert_eq!(method.kind, NodeKind::Method);
        assert_eq!(method.signature.as_deref(), Some("void (int amount)"));

        let field = result.nodes.iter().find(|n| n.name == "count").unwrap();
        assert_eq!(field.kind, NodeKind::Field);

        let iface = result.nodes.iter().find(|n| n.name == "Validator").unwrap();
        assert_eq!(iface.kind, NodeKind::Interface);

        let status = result.nodes.iter().find(|n| n.name == "Status").unwrap();
        assert_eq!(status.kind, NodeKind::Enum);
        let open = result.nodes.iter().find(|n| n.name == "OPEN").unwrap();
        assert_eq!(open.kind, NodeKind::EnumMember);

        let import = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Import)
            .expect("import node");
        assert_eq!(import.name, "java.util.List");

        // package_declaration wraps top-level declarations in a namespace node
        let ns = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Namespace)
            .expect("namespace node");
        assert_eq!(ns.name, "com.example.app");
    }
}

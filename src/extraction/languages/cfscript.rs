//! CFScript language extraction configuration.
//!
//! CFScript is the script dialect embedded in CFML and used directly by
//! modern `.cfc`, `.cfm`, and `.cfs` files.

use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{
    ClassLikeKind,
    ImportInfo,
    ImportOutcome,
    LanguageExtractor,
    SyntaxNode,
};
use crate::types::Visibility;

pub struct CfscriptExtractor;

impl LanguageExtractor for CfscriptExtractor {
    fn function_types(&self) -> &[&str] {
        &[
            "function_declaration",
            "function_expression",
            "arrow_function",
        ]
    }

    fn class_types(&self) -> &[&str] {
        &["component"]
    }

    fn method_types(&self) -> &[&str] {
        &["function_declaration", "method_definition"]
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
        &["import_statement", "include_statement"]
    }

    fn call_types(&self) -> &[&str] {
        &["call_expression"]
    }

    fn variable_types(&self) -> &[&str] {
        &["variable_declaration"]
    }

    fn property_types(&self) -> &[&str] {
        &["property_declaration"]
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

    fn classify_class_node(&self, node: SyntaxNode<'_>, _source: &str) -> ClassLikeKind {
        if node
            .child(0)
            .is_some_and(|child| child.kind() == "interface")
        {
            ClassLikeKind::Interface
        } else {
            ClassLikeKind::Class
        }
    }

    fn get_visibility(&self, node: SyntaxNode<'_>, source: &str) -> Option<Visibility> {
        for index in 0..node.child_count() as u32 {
            let Some(child) = node.child(index) else {
                continue;
            };
            if child.kind() != "access_type" {
                continue;
            }
            return match get_node_text(child, source).trim() {
                "public" | "remote" => Some(Visibility::Public),
                "private" => Some(Visibility::Private),
                "package" => Some(Visibility::Internal),
                _ => None,
            };
        }
        None
    }

    fn get_signature(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        let params = get_child_by_field(node, "parameters")?;
        Some(get_node_text(params, source).to_string())
    }

    fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
        let signature = get_node_text(node, source).trim().to_string();

        if node.kind() == "include_statement" {
            for index in 0..node.named_child_count() as u32 {
                let Some(child) = node.named_child(index) else {
                    continue;
                };
                if child.kind() != "string" {
                    continue;
                }
                let module_name = strip_quotes(get_node_text(child, source));
                return if module_name.is_empty() {
                    ImportOutcome::Declined
                } else {
                    ImportOutcome::Info(ImportInfo::new(module_name, signature))
                };
            }
            return ImportOutcome::Declined;
        }

        let Some(source_node) = get_child_by_field(node, "source") else {
            return ImportOutcome::Declined;
        };
        let module_name = if source_node.kind() == "import_path" {
            let mut segments = Vec::new();
            for index in 0..source_node.named_child_count() as u32 {
                if let Some(child) = source_node.named_child(index) {
                    segments.push(get_node_text(child, source));
                }
            }
            segments.join(".")
        } else {
            strip_quotes(get_node_text(source_node, source)).to_string()
        };

        if module_name.is_empty() {
            ImportOutcome::Declined
        } else {
            ImportOutcome::Info(ImportInfo::new(module_name, signature))
        }
    }
}

fn strip_quotes(value: &str) -> &str {
    let value = value
        .strip_prefix('\'')
        .or_else(|| value.strip_prefix('"'))
        .unwrap_or(value);
    value
        .strip_suffix('\'')
        .or_else(|| value.strip_suffix('"'))
        .unwrap_or(value)
}

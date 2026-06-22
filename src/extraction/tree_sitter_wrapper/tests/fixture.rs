use super::super::TreeSitterExtractor;
use crate::extraction::tree_sitter_helpers::get_node_text;
use crate::extraction::tree_sitter_types::{
    ImportInfo,
    ImportOutcome,
    LanguageExtractor,
    SyntaxNode,
};
use crate::types::{ExtractionResult, Language};

struct TsTestExtractor;

impl LanguageExtractor for TsTestExtractor {
    fn function_types(&self) -> &[&str] {
        &[
            "function_declaration",
            "arrow_function",
            "function_expression",
        ]
    }

    fn class_types(&self) -> &[&str] {
        &["class_declaration", "abstract_class_declaration"]
    }

    fn method_types(&self) -> &[&str] {
        &["method_definition", "public_field_definition"]
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
        &["property_identifier", "enum_assignment"]
    }

    fn type_alias_types(&self) -> &[&str] {
        &["type_alias_declaration"]
    }

    fn import_types(&self) -> &[&str] {
        &["import_statement"]
    }

    fn call_types(&self) -> &[&str] {
        &["call_expression"]
    }

    fn variable_types(&self) -> &[&str] {
        &["lexical_declaration", "variable_declaration"]
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

    fn is_exported(&self, node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
        let mut current = node.parent();
        while let Some(p) = current {
            if p.kind() == "export_statement" {
                return Some(true);
            }
            current = p.parent();
        }
        Some(false)
    }

    fn is_const(&self, node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
        if node.kind() == "lexical_declaration" {
            for i in 0..node.child_count() as u32 {
                if let Some(c) = node.child(i) {
                    if c.kind() == "const" {
                        return Some(true);
                    }
                }
            }
        }
        Some(false)
    }

    fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
        if let Some(source_field) = node.child_by_field_name("source") {
            let module_name = get_node_text(source_field, source).replace(['\'', '"'], "");
            if !module_name.is_empty() {
                return ImportOutcome::Info(ImportInfo::new(
                    module_name,
                    get_node_text(node, source).trim(),
                ));
            }
        }
        ImportOutcome::Declined
    }
}

pub(super) fn extract_ts(file_path: &str, source: &str) -> ExtractionResult {
    let ext = TsTestExtractor;
    TreeSitterExtractor::new(file_path, source, Some(Language::Typescript), Some(&ext)).extract()
}

//! JavaScript language extraction config.
//!
//! Ported from `src/extraction/languages/javascript.ts`.

use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{
    ImportInfo,
    ImportOutcome,
    LanguageExtractor,
    SyntaxNode,
};

pub struct JavascriptExtractor;

impl LanguageExtractor for JavascriptExtractor {
    fn function_types(&self) -> &[&str] {
        &[
            "function_declaration",
            "arrow_function",
            "function_expression",
        ]
    }
    fn class_types(&self) -> &[&str] {
        &["class_declaration"]
    }
    fn method_types(&self) -> &[&str] {
        &["method_definition", "field_definition"]
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

    fn resolve_body<'t>(&self, node: SyntaxNode<'t>, body_field: &str) -> Option<SyntaxNode<'t>> {
        // field_definition (arrow function class fields) nest the body inside
        // an arrow_function or function_expression child:
        //   field_definition → arrow_function → body (statement_block)
        // Also handles wrapper patterns like: field = throttle((e) => { ... })
        //   field_definition → call_expression → arguments → arrow_function → body
        if node.kind() == "field_definition" {
            for i in 0..node.named_child_count() as u32 {
                let Some(child) = node.named_child(i) else {
                    continue;
                };
                if child.kind() == "arrow_function" || child.kind() == "function_expression" {
                    return get_child_by_field(child, body_field);
                }
                if child.kind() == "call_expression" {
                    if let Some(args) = get_child_by_field(child, "arguments") {
                        for j in 0..args.named_child_count() as u32 {
                            if let Some(arg) = args.named_child(j) {
                                if arg.kind() == "arrow_function"
                                    || arg.kind() == "function_expression"
                                {
                                    return get_child_by_field(arg, body_field);
                                }
                            }
                        }
                    }
                }
            }
        }
        None
    }

    fn get_signature(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        let params = get_child_by_field(node, "parameters")?;
        Some(get_node_text(params, source).to_string())
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

    fn is_const(&self, node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
        if node.kind() == "lexical_declaration" {
            for i in 0..node.child_count() as u32 {
                if let Some(child) = node.child(i) {
                    if child.kind() == "const" {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
    use crate::types::{Language, NodeKind};

    #[test]
    fn javascript_smoke_extraction() {
        let source = "import { helper } from './util.js';\n\nexport async function main() {\n  helper();\n}\n\nclass Widget {\n  render() {\n    main();\n  }\n}\n\nconst MAX = 5;\nlet count = 0;\n";
        let result = TreeSitterExtractor::new(
            "src/app.js",
            source,
            Some(Language::Javascript),
            Some(&JavascriptExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let func = result.nodes.iter().find(|n| n.name == "main").unwrap();
        assert_eq!(func.kind, NodeKind::Function);
        assert_eq!(func.is_exported, Some(true));
        assert_eq!(func.is_async, Some(true));
        assert_eq!(func.signature.as_deref(), Some("()"));

        let class = result.nodes.iter().find(|n| n.name == "Widget").unwrap();
        assert_eq!(class.kind, NodeKind::Class);
        let method = result.nodes.iter().find(|n| n.name == "render").unwrap();
        assert_eq!(method.kind, NodeKind::Method);

        let constant = result.nodes.iter().find(|n| n.name == "MAX").unwrap();
        assert_eq!(constant.kind, NodeKind::Constant);
        let variable = result.nodes.iter().find(|n| n.name == "count").unwrap();
        assert_eq!(variable.kind, NodeKind::Variable);

        let import = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Import)
            .expect("import node");
        assert_eq!(import.name, "./util.js");
    }
}

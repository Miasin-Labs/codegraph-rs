//! ArkTS (HarmonyOS / OpenHarmony) extraction configuration.

use std::collections::HashSet;

use crate::extraction::languages::typescript::{TypescriptExtractor, classify_ts_class_member};
use crate::extraction::tree_sitter_helpers::get_node_text;
use crate::extraction::tree_sitter_types::{
    ClassMemberKind,
    ImportOutcome,
    LanguageExtractor,
    SyntaxNode,
};
use crate::types::Visibility;

pub struct ArktsExtractor;

const DECORATED_MEMBER_TYPES: &[&str] = &[
    "struct_declaration",
    "public_field_definition",
    "method_definition",
    "function_declaration",
];

fn decorator_name(node: SyntaxNode<'_>, source: &str) -> Option<String> {
    for i in 0..node.named_child_count() as u32 {
        let child = node.named_child(i)?;
        if child.kind() == "identifier" {
            return Some(get_node_text(child, source).to_string());
        }
        if child.kind() == "call_expression" {
            let function = child.child_by_field_name("function")?;
            if function.kind() == "identifier" {
                return Some(get_node_text(function, source).to_string());
            }
        }
    }
    None
}

fn collect_decorator_names(node: SyntaxNode<'_>, source: &str) -> Option<Vec<String>> {
    let mut names = Vec::new();

    for i in 0..node.named_child_count() as u32 {
        let Some(child) = node.named_child(i) else {
            continue;
        };
        if child.kind() == "decorator" {
            if let Some(name) = decorator_name(child, source) {
                names.push(name);
            }
        }
    }

    let mut preceding = Vec::new();
    let mut sibling = node.prev_named_sibling();
    while let Some(current) = sibling {
        if current.kind() != "decorator" {
            break;
        }
        if let Some(name) = decorator_name(current, source) {
            preceding.push(name);
        }
        sibling = current.prev_named_sibling();
    }
    preceding.reverse();
    preceding.extend(names);

    let mut seen = HashSet::new();
    preceding.retain(|name| seen.insert(name.clone()));
    (!preceding.is_empty()).then_some(preceding)
}

impl LanguageExtractor for ArktsExtractor {
    fn function_types(&self) -> &[&str] {
        TypescriptExtractor.function_types()
    }
    fn class_types(&self) -> &[&str] {
        TypescriptExtractor.class_types()
    }
    fn method_types(&self) -> &[&str] {
        TypescriptExtractor.method_types()
    }
    fn interface_types(&self) -> &[&str] {
        TypescriptExtractor.interface_types()
    }
    fn struct_types(&self) -> &[&str] {
        &["struct_declaration"]
    }
    fn enum_types(&self) -> &[&str] {
        TypescriptExtractor.enum_types()
    }
    fn enum_member_types(&self) -> &[&str] {
        TypescriptExtractor.enum_member_types()
    }
    fn type_alias_types(&self) -> &[&str] {
        TypescriptExtractor.type_alias_types()
    }
    fn import_types(&self) -> &[&str] {
        TypescriptExtractor.import_types()
    }
    fn call_types(&self) -> &[&str] {
        &[
            "call_expression",
            "arkui_component_expression",
            "leading_dot_expression",
        ]
    }
    fn variable_types(&self) -> &[&str] {
        TypescriptExtractor.variable_types()
    }
    fn name_field(&self) -> &str {
        TypescriptExtractor.name_field()
    }
    fn body_field(&self) -> &str {
        TypescriptExtractor.body_field()
    }
    fn params_field(&self) -> &str {
        TypescriptExtractor.params_field()
    }
    fn return_field(&self) -> Option<&str> {
        TypescriptExtractor.return_field()
    }
    fn resolve_body<'t>(&self, node: SyntaxNode<'t>, field: &str) -> Option<SyntaxNode<'t>> {
        TypescriptExtractor.resolve_body(node, field)
    }
    fn get_signature(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        TypescriptExtractor.get_signature(node, source)
    }
    fn get_visibility(&self, node: SyntaxNode<'_>, source: &str) -> Option<Visibility> {
        TypescriptExtractor.get_visibility(node, source)
    }
    fn is_exported(&self, node: SyntaxNode<'_>, source: &str) -> Option<bool> {
        TypescriptExtractor.is_exported(node, source)
    }
    fn is_async(&self, node: SyntaxNode<'_>, source: &str) -> Option<bool> {
        TypescriptExtractor.is_async(node, source)
    }
    fn is_static(&self, node: SyntaxNode<'_>, source: &str) -> Option<bool> {
        TypescriptExtractor.is_static(node, source)
    }
    fn is_const(&self, node: SyntaxNode<'_>, source: &str) -> Option<bool> {
        TypescriptExtractor.is_const(node, source)
    }
    fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
        TypescriptExtractor.extract_import(node, source)
    }
    fn classify_method_node(&self, node: SyntaxNode<'_>, _source: &str) -> ClassMemberKind {
        classify_ts_class_member(node)
    }
    fn extract_modifiers(&self, node: SyntaxNode<'_>, source: &str) -> Option<Vec<String>> {
        DECORATED_MEMBER_TYPES
            .contains(&node.kind())
            .then(|| collect_decorator_names(node, source))
            .flatten()
    }
}

//! C and C++ language extraction configs.
//!
//! Ported from `src/extraction/languages/c-cpp.ts`.

use std::collections::VecDeque;

use super::named_children;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{
    ImportInfo,
    ImportOutcome,
    LanguageExtractor,
    SyntaxNode,
};
use crate::types::{NodeKind, Visibility};

fn extract_cpp_qualified_method_name(node: SyntaxNode<'_>, source: &str) -> Option<String> {
    let declarator = get_child_by_field(node, "declarator")?;

    let mut queue: VecDeque<SyntaxNode<'_>> = VecDeque::from([declarator]);
    while let Some(current) = queue.pop_front() {
        if current.kind() == "qualified_identifier" {
            let text = get_node_text(current, source).trim().to_string();
            let parts: Vec<&str> = text.split("::").filter(|p| !p.is_empty()).collect();
            return parts.last().map(|p| p.to_string());
        }
        for i in 0..current.named_child_count() as u32 {
            if let Some(child) = current.named_child(i) {
                queue.push_back(child);
            }
        }
    }

    None
}

fn extract_cpp_receiver_type(node: SyntaxNode<'_>, source: &str) -> Option<String> {
    let declarator = get_child_by_field(node, "declarator")?;

    let mut queue: VecDeque<SyntaxNode<'_>> = VecDeque::from([declarator]);
    while let Some(current) = queue.pop_front() {
        if current.kind() == "qualified_identifier" {
            let text = get_node_text(current, source).trim().to_string();
            let parts: Vec<&str> = text.split("::").filter(|p| !p.is_empty()).collect();
            if parts.len() > 1 {
                return Some(parts[..parts.len() - 1].join("::"));
            }
            return None;
        }
        for i in 0..current.named_child_count() as u32 {
            if let Some(child) = current.named_child(i) {
                queue.push_back(child);
            }
        }
    }

    None
}

/// Shared `extractImport` body for C / C++ / (also reused by ObjC in TS shape):
/// `#include <stdio.h>` / `#include "myheader.h"`.
fn extract_include_import(node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
    let import_text = get_node_text(node, source).trim();
    if let Some(system_lib) = named_children(node)
        .into_iter()
        .find(|c| c.kind() == "system_lib_string")
    {
        // TS: .replace(/^<|>$/g, '')
        let text = get_node_text(system_lib, source);
        let text = text.strip_prefix('<').unwrap_or(text);
        let text = text.strip_suffix('>').unwrap_or(text);
        return ImportOutcome::Info(ImportInfo::new(text, import_text));
    }
    if let Some(string_literal) = named_children(node)
        .into_iter()
        .find(|c| c.kind() == "string_literal")
    {
        if let Some(string_content) = named_children(string_literal)
            .into_iter()
            .find(|c| c.kind() == "string_content")
        {
            return ImportOutcome::Info(ImportInfo::new(
                get_node_text(string_content, source),
                import_text,
            ));
        }
    }
    ImportOutcome::Declined
}

/// C typedef: `typedef enum { ... } name;` or `typedef struct { ... } name;`
/// The inner enum_specifier/struct_specifier is anonymous, but we want the
/// typedef name to become the enum/struct node name.
fn resolve_typedef_kind(node: SyntaxNode<'_>) -> Option<NodeKind> {
    for i in 0..node.named_child_count() as u32 {
        let Some(child) = node.named_child(i) else {
            continue;
        };
        if child.kind() == "enum_specifier" && get_child_by_field(child, "body").is_some() {
            return Some(NodeKind::Enum);
        }
        if child.kind() == "struct_specifier" && get_child_by_field(child, "body").is_some() {
            return Some(NodeKind::Struct);
        }
    }
    None
}

pub struct CExtractor;

impl LanguageExtractor for CExtractor {
    fn function_types(&self) -> &[&str] {
        &["function_definition"]
    }
    fn class_types(&self) -> &[&str] {
        &[]
    }
    fn method_types(&self) -> &[&str] {
        &[]
    }
    fn interface_types(&self) -> &[&str] {
        &[]
    }
    fn struct_types(&self) -> &[&str] {
        &["struct_specifier"]
    }
    fn enum_types(&self) -> &[&str] {
        &["enum_specifier"]
    }
    fn enum_member_types(&self) -> &[&str] {
        &["enumerator"]
    }
    fn type_alias_types(&self) -> &[&str] {
        // typedef
        &["type_definition"]
    }
    fn import_types(&self) -> &[&str] {
        &["preproc_include"]
    }
    fn call_types(&self) -> &[&str] {
        &["call_expression"]
    }
    fn variable_types(&self) -> &[&str] {
        &["declaration"]
    }
    fn name_field(&self) -> &str {
        "declarator"
    }
    fn body_field(&self) -> &str {
        "body"
    }
    fn params_field(&self) -> &str {
        "parameters"
    }

    fn resolve_type_alias_kind(&self, node: SyntaxNode<'_>, _source: &str) -> Option<NodeKind> {
        resolve_typedef_kind(node)
    }

    fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
        extract_include_import(node, source)
    }
}

pub struct CppExtractor;

impl LanguageExtractor for CppExtractor {
    fn function_types(&self) -> &[&str] {
        &["function_definition"]
    }
    fn class_types(&self) -> &[&str] {
        &["class_specifier"]
    }
    fn method_types(&self) -> &[&str] {
        &["function_definition"]
    }
    fn interface_types(&self) -> &[&str] {
        &[]
    }
    fn struct_types(&self) -> &[&str] {
        &["struct_specifier"]
    }
    fn enum_types(&self) -> &[&str] {
        &["enum_specifier"]
    }
    fn enum_member_types(&self) -> &[&str] {
        &["enumerator"]
    }
    fn type_alias_types(&self) -> &[&str] {
        // typedef and using
        &["type_definition", "alias_declaration"]
    }
    fn import_types(&self) -> &[&str] {
        &["preproc_include"]
    }
    fn call_types(&self) -> &[&str] {
        &["call_expression"]
    }
    fn variable_types(&self) -> &[&str] {
        &["declaration"]
    }
    fn name_field(&self) -> &str {
        "declarator"
    }
    fn body_field(&self) -> &str {
        "body"
    }
    fn params_field(&self) -> &str {
        "parameters"
    }

    fn resolve_name(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        extract_cpp_qualified_method_name(node, source)
    }

    fn get_receiver_type(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        extract_cpp_receiver_type(node, source)
    }

    fn get_visibility(&self, node: SyntaxNode<'_>, source: &str) -> Option<Visibility> {
        // Check for access specifier in parent
        if let Some(parent) = node.parent() {
            for i in 0..parent.child_count() as u32 {
                if let Some(child) = parent.child(i) {
                    if child.kind() == "access_specifier" {
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
        }
        None
    }

    fn resolve_type_alias_kind(&self, node: SyntaxNode<'_>, _source: &str) -> Option<NodeKind> {
        // C++ typedef: `typedef enum { ... } name;` or `typedef struct { ... } name;`
        resolve_typedef_kind(node)
    }

    fn is_misparsed_function(&self, name: &str, _node: SyntaxNode<'_>) -> bool {
        // C++ macros like NLOHMANN_JSON_NAMESPACE_BEGIN cause tree-sitter to misparse
        // namespace blocks as function_definitions (e.g. name = "namespace detail").
        // Also filter C++ keywords that tree-sitter occasionally misinterprets as
        // function/method names (e.g. switch statements inside macro-confused scopes).
        if name.starts_with("namespace") {
            return true;
        }
        const CPP_KEYWORDS: [&str; 7] = ["switch", "if", "for", "while", "do", "case", "return"];
        CPP_KEYWORDS.contains(&name)
    }

    fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
        extract_include_import(node, source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
    use crate::types::{Language, NodeKind};

    #[test]
    fn c_smoke_extraction() {
        let source = "#include <stdio.h>\n#include \"local.h\"\n\nstruct Point {\n    int x;\n    int y;\n};\n\ntypedef enum { RED, GREEN } Color;\n\nint main(void) {\n    printf(\"hi\");\n    return 0;\n}\n";
        let result =
            TreeSitterExtractor::new("src/main.c", source, Some(Language::C), Some(&CExtractor))
                .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let point = result.nodes.iter().find(|n| n.name == "Point").unwrap();
        assert_eq!(point.kind, NodeKind::Struct);

        let color = result.nodes.iter().find(|n| n.name == "Color").unwrap();
        // typedef enum resolves to enum kind via resolve_type_alias_kind
        assert_eq!(color.kind, NodeKind::Enum);

        let main = result.nodes.iter().find(|n| n.name == "main").unwrap();
        assert_eq!(main.kind, NodeKind::Function);

        let imports: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Import)
            .collect();
        assert!(imports.iter().any(|n| n.name == "stdio.h"));
        assert!(imports.iter().any(|n| n.name == "local.h"));
    }

    #[test]
    fn cpp_smoke_extraction() {
        let source = "#include <iostream>\n\nclass Engine {\npublic:\n    void start();\n};\n\nvoid Engine::start() {\n    helper();\n}\n\nvoid helper() {}\n";
        let result = TreeSitterExtractor::new(
            "src/engine.cpp",
            source,
            Some(Language::Cpp),
            Some(&CppExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let class = result.nodes.iter().find(|n| n.name == "Engine").unwrap();
        assert_eq!(class.kind, NodeKind::Class);

        // Out-of-line definition resolves to the unqualified name via resolve_name,
        // with the receiver from the qualified identifier.
        let start = result
            .nodes
            .iter()
            .find(|n| n.name == "start" && n.kind != NodeKind::File)
            .expect("start method");
        assert!(
            start.qualified_name.contains("Engine"),
            "expected receiver in qualified name, got {:?}",
            start.qualified_name
        );

        let import = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Import)
            .unwrap();
        assert_eq!(import.name, "iostream");
    }

    #[test]
    fn cpp_misparsed_function_filter() {
        let ext = CppExtractor;
        let source = "void helper() {}\n";
        let mut parser = crate::extraction::grammars::create_parser(Language::Cpp).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let node = tree.root_node().named_child(0).unwrap();
        assert!(ext.is_misparsed_function("namespace detail", node));
        assert!(ext.is_misparsed_function("switch", node));
        assert!(ext.is_misparsed_function("return", node));
        assert!(!ext.is_misparsed_function("helper", node));
    }
}

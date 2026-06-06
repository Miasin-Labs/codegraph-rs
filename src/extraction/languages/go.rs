//! Go language extraction config.
//!
//! Ported from `src/extraction/languages/go.ts`.

use std::sync::LazyLock;

use regex::Regex;

use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{LanguageExtractor, SyntaxNode};
use crate::types::NodeKind;

/// Extract type name from "(sl *Type)", "(sl Type)", "(*Type)", "(Type)" and
/// generic receivers "(s *Stack[T])". Anchor on the opening "(" and skip an
/// optional receiver var name; the old `name)`-anchored pattern never matched
/// the `[T])` suffix, so generic-type methods were orphaned from their type
/// (no struct→method `contains` edge). (#583)
static RECEIVER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\(\s*(?:[A-Za-z_]\w*\s+)?\*?\s*([A-Za-z_]\w*)").expect("valid regex")
});

pub struct GoExtractor;

impl LanguageExtractor for GoExtractor {
    fn function_types(&self) -> &[&str] {
        &["function_declaration"]
    }
    fn class_types(&self) -> &[&str] {
        // Go doesn't have classes
        &[]
    }
    fn method_types(&self) -> &[&str] {
        &["method_declaration"]
    }
    fn interface_types(&self) -> &[&str] {
        // Handled via type_spec → resolve_type_alias_kind
        &[]
    }
    fn struct_types(&self) -> &[&str] {
        // Handled via type_spec → resolve_type_alias_kind
        &[]
    }
    fn enum_types(&self) -> &[&str] {
        &[]
    }
    fn type_alias_types(&self) -> &[&str] {
        // Go type declarations
        &["type_spec"]
    }
    fn import_types(&self) -> &[&str] {
        &["import_declaration"]
    }
    fn call_types(&self) -> &[&str] {
        &["call_expression"]
    }
    fn variable_types(&self) -> &[&str] {
        &[
            "var_declaration",
            "short_var_declaration",
            "const_declaration",
        ]
    }
    fn methods_are_top_level(&self) -> bool {
        true
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
        Some("result")
    }

    fn get_signature(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        let params = get_child_by_field(node, "parameters")?;
        let result = get_child_by_field(node, "result");
        let mut sig = get_node_text(params, source).to_string();
        if let Some(result) = result {
            sig.push(' ');
            sig.push_str(get_node_text(result, source));
        }
        Some(sig)
    }

    fn resolve_type_alias_kind(&self, node: SyntaxNode<'_>, _source: &str) -> Option<NodeKind> {
        // Go type_spec: `type Foo struct { ... }` or `type Bar interface { ... }`
        // The inner type is in the 'type' field of the type_spec node
        let type_child = get_child_by_field(node, "type")?;
        match type_child.kind() {
            "struct_type" => Some(NodeKind::Struct),
            "interface_type" => Some(NodeKind::Interface),
            _ => None,
        }
    }

    fn is_exported(&self, node: SyntaxNode<'_>, source: &str) -> Option<bool> {
        // Go: a symbol is exported when its identifier starts with an uppercase letter.
        // Look at the `name` field directly (works for function_declaration,
        // method_declaration, type_spec, and var_spec / const_spec via extractor flow).
        if let Some(name_node) = get_child_by_field(node, "name") {
            let text = get_node_text(name_node, source);
            // TS: charCodeAt(0) >= 65 && <= 90 (A-Z)
            return Some(
                text.as_bytes()
                    .first()
                    .map(|b| b.is_ascii_uppercase())
                    .unwrap_or(false),
            );
        }
        Some(false)
    }

    fn get_receiver_type(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        // Go method_declaration has a "receiver" field: func (sl *scrapeLoop) run(...)
        // The receiver is a parameter_list containing a parameter_declaration
        // with a type that may be a pointer_type (*scrapeLoop) or plain type (scrapeLoop)
        let receiver = get_child_by_field(node, "receiver")?;
        let text = get_node_text(receiver, source);
        RECEIVER_RE
            .captures(text)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
    use crate::types::{Language, NodeKind};

    #[test]
    fn go_smoke_extraction() {
        let source = "package main\n\nimport \"fmt\"\n\ntype Server struct {\n\taddr string\n}\n\ntype Handler interface {\n\tServe()\n}\n\nfunc (s *Server) Start() error {\n\tfmt.Println(s.addr)\n\treturn nil\n}\n\nfunc helper() {}\n";
        let result = TreeSitterExtractor::new(
            "src/main.go",
            source,
            Some(Language::Go),
            Some(&GoExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let server = result.nodes.iter().find(|n| n.name == "Server").unwrap();
        assert_eq!(server.kind, NodeKind::Struct);
        assert_eq!(server.is_exported, Some(true));

        let handler = result.nodes.iter().find(|n| n.name == "Handler").unwrap();
        assert_eq!(handler.kind, NodeKind::Interface);

        let start = result.nodes.iter().find(|n| n.name == "Start").unwrap();
        assert_eq!(start.kind, NodeKind::Method);
        // Receiver type included in the qualified name (methods_are_top_level)
        assert!(
            start.qualified_name.contains("Server"),
            "qualified name {:?} should include receiver type",
            start.qualified_name
        );

        let helper = result.nodes.iter().find(|n| n.name == "helper").unwrap();
        assert_eq!(helper.kind, NodeKind::Function);
        assert_eq!(helper.is_exported, Some(false));
    }

    #[test]
    fn go_receiver_regex_handles_generics() {
        // Direct regex spot-checks mirroring the #583 fix
        for (text, expected) in [
            ("(sl *scrapeLoop)", "scrapeLoop"),
            ("(sl scrapeLoop)", "scrapeLoop"),
            ("(*Type)", "Type"),
            ("(Type)", "Type"),
            ("(s *Stack[T])", "Stack"),
        ] {
            let cap = RECEIVER_RE
                .captures(text)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str())
                .unwrap_or("");
            assert_eq!(cap, expected, "for {text}");
        }
    }
}

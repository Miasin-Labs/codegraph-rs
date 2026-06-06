//! Dart language extraction config.
//!
//! Ported from `src/extraction/languages/dart.ts`.

use super::{find_named_child, named_children};
use crate::extraction::tree_sitter_helpers::get_node_text;
use crate::extraction::tree_sitter_types::{
    ImportInfo,
    ImportOutcome,
    LanguageExtractor,
    SyntaxNode,
};
use crate::types::Visibility;

pub struct DartExtractor;

impl LanguageExtractor for DartExtractor {
    fn function_types(&self) -> &[&str] {
        &["function_signature"]
    }
    fn class_types(&self) -> &[&str] {
        // TS (older wasm grammar): `class_definition`. The native
        // tree-sitter-dart 0.2 grammar names the node `class_declaration`;
        // both are listed so the config stays a superset of the TS one.
        &["class_definition", "class_declaration"]
    }
    fn method_types(&self) -> &[&str] {
        &["method_signature"]
    }
    fn interface_types(&self) -> &[&str] {
        &[]
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
        &["type_alias"]
    }
    fn import_types(&self) -> &[&str] {
        &["import_or_export"]
    }
    fn call_types(&self) -> &[&str] {
        // TS (older wasm grammar): [] — calls were identifier+selector pairs,
        // handled via extract_bare_call. The native tree-sitter-dart 0.2
        // grammar emits real `call_expression` nodes, so they are claimed here
        // to keep capturing Dart calls; the selector-based extract_bare_call
        // logic is retained for grammar shapes that still use it.
        &["call_expression"]
    }
    fn variable_types(&self) -> &[&str] {
        &[]
    }
    fn extra_class_node_types(&self) -> &[&str] {
        &["mixin_declaration", "extension_declaration"]
    }
    fn name_field(&self) -> &str {
        "name"
    }
    fn body_field(&self) -> &str {
        // class_definition uses 'body' field
        "body"
    }
    fn params_field(&self) -> &str {
        "formal_parameter_list"
    }
    fn return_field(&self) -> Option<&str> {
        Some("type")
    }

    fn resolve_body<'t>(&self, node: SyntaxNode<'t>, body_field: &str) -> Option<SyntaxNode<'t>> {
        // Dart: function_body is a next sibling of function_signature/method_signature
        if node.kind() == "function_signature" || node.kind() == "method_signature" {
            let next = node.next_named_sibling();
            if let Some(next) = next {
                if next.kind() == "function_body" {
                    return Some(next);
                }
            }
            return None;
        }
        // For class/mixin/extension: try standard field, then class_body/extension_body
        if let Some(standard) = node.child_by_field_name(body_field) {
            return Some(standard);
        }
        named_children(node)
            .into_iter()
            .find(|c| c.kind() == "class_body" || c.kind() == "extension_body")
    }

    fn get_signature(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        // For function_signature: extract params + return type
        // For method_signature: delegate to inner function_signature
        let mut sig = node;
        if node.kind() == "method_signature" {
            if let Some(inner) = named_children(node).into_iter().find(|c| {
                matches!(
                    c.kind(),
                    "function_signature" | "getter_signature" | "setter_signature"
                )
            }) {
                sig = inner;
            }
        }
        let params = named_children(sig)
            .into_iter()
            .find(|c| c.kind() == "formal_parameter_list");
        let ret_type = named_children(sig)
            .into_iter()
            .find(|c| c.kind() == "type_identifier" || c.kind() == "void_type");
        if params.is_none() && ret_type.is_none() {
            return None;
        }
        let mut result = String::new();
        if let Some(rt) = ret_type {
            result.push_str(get_node_text(rt, source));
            result.push(' ');
        }
        if let Some(p) = params {
            result.push_str(get_node_text(p, source));
        }
        let result = result.trim().to_string();
        if result.is_empty() {
            None
        } else {
            Some(result)
        }
    }

    fn get_visibility(&self, node: SyntaxNode<'_>, source: &str) -> Option<Visibility> {
        // Dart convention: _ prefix means private, otherwise public
        let mut name_node: Option<SyntaxNode<'_>> = None;
        if node.kind() == "method_signature" {
            if let Some(inner) = named_children(node).into_iter().find(|c| {
                matches!(
                    c.kind(),
                    "function_signature" | "getter_signature" | "setter_signature"
                )
            }) {
                name_node = find_named_child(inner, "identifier");
            }
        } else {
            name_node = node.child_by_field_name("name");
        }
        if let Some(n) = name_node {
            if get_node_text(n, source).starts_with('_') {
                return Some(Visibility::Private);
            }
        }
        Some(Visibility::Public)
    }

    fn is_async(&self, node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
        // In Dart, 'async' is on the function_body (next sibling), not the signature
        if let Some(next_sibling) = node.next_named_sibling() {
            if next_sibling.kind() == "function_body" {
                for i in 0..next_sibling.child_count() as u32 {
                    if let Some(child) = next_sibling.child(i) {
                        if child.kind() == "async" {
                            return Some(true);
                        }
                    }
                }
            }
        }
        Some(false)
    }

    fn is_static(&self, node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
        // For method_signature, check for 'static' child
        if node.kind() == "method_signature" {
            for i in 0..node.child_count() as u32 {
                if let Some(child) = node.child(i) {
                    if child.kind() == "static" {
                        return Some(true);
                    }
                }
            }
        }
        Some(false)
    }

    fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
        let import_text = get_node_text(node, source).trim();
        let mut module_name = String::new();

        // Dart imports: import 'dart:async'; import 'package:foo/bar.dart' as bar;
        if let Some(library_import) = find_named_child(node, "library_import") {
            if let Some(import_spec) = find_named_child(library_import, "import_specification") {
                if let Some(configurable_uri) = find_named_child(import_spec, "configurable_uri") {
                    if let Some(uri) = find_named_child(configurable_uri, "uri") {
                        if let Some(string_literal) = find_named_child(uri, "string_literal") {
                            module_name =
                                get_node_text(string_literal, source).replace(['\'', '"'], "");
                        }
                    }
                }
            }
        }

        // Also handle exports: export 'src/foo.dart';
        if module_name.is_empty() {
            if let Some(library_export) = find_named_child(node, "library_export") {
                if let Some(configurable_uri) = find_named_child(library_export, "configurable_uri")
                {
                    if let Some(uri) = find_named_child(configurable_uri, "uri") {
                        if let Some(string_literal) = find_named_child(uri, "string_literal") {
                            module_name =
                                get_node_text(string_literal, source).replace(['\'', '"'], "");
                        }
                    }
                }
            }
        }

        if !module_name.is_empty() {
            return ImportOutcome::Info(ImportInfo::new(module_name, import_text));
        }
        ImportOutcome::Declined
    }

    fn extract_bare_call(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        // Dart calls are: identifier + selector(argument_part), not a dedicated call node.
        // Match on selector nodes that contain argument_part.
        if node.kind() == "selector" {
            let has_arg_part = named_children(node)
                .into_iter()
                .any(|c| c.kind() == "argument_part");
            if !has_arg_part {
                return None;
            }

            let prev = node.prev_named_sibling()?;

            // Simple function/constructor call: prev is identifier (e.g., runApp(...), MyWidget(...))
            if prev.kind() == "identifier" {
                return Some(get_node_text(prev, source).to_string());
            }

            // Method call: prev is selector with accessor (e.g., obj.method(...), Navigator.push(...))
            if prev.kind() == "selector" {
                if let Some(accessor) = named_children(prev).into_iter().find(|c| {
                    c.kind() == "unconditional_assignable_selector"
                        || c.kind() == "conditional_assignable_selector"
                }) {
                    if let Some(method_id) = find_named_child(accessor, "identifier") {
                        // Include receiver for first call in chain (receiver is a direct identifier)
                        if let Some(accessor_prev) = prev.prev_named_sibling() {
                            if accessor_prev.kind() == "identifier" {
                                return Some(format!(
                                    "{}.{}",
                                    get_node_text(accessor_prev, source),
                                    get_node_text(method_id, source)
                                ));
                            }
                        }
                        return Some(get_node_text(method_id, source).to_string());
                    }
                }
            }

            // super.method() / this.method(): prev is bare unconditional_assignable_selector
            if prev.kind() == "unconditional_assignable_selector"
                || prev.kind() == "conditional_assignable_selector"
            {
                if let Some(method_id) = find_named_child(prev, "identifier") {
                    return Some(get_node_text(method_id, source).to_string());
                }
            }

            return None;
        }

        // new MyWidget() — explicit constructor call
        if node.kind() == "new_expression" {
            if let Some(type_id) = find_named_child(node, "type_identifier") {
                return Some(get_node_text(type_id, source).to_string());
            }
            return None;
        }

        // const EdgeInsets.all(8.0) — const constructor call
        if node.kind() == "const_object_expression" {
            let type_id = find_named_child(node, "type_identifier");
            let name_id = find_named_child(node, "identifier");
            if let (Some(type_id), Some(name_id)) = (type_id, name_id) {
                return Some(format!(
                    "{}.{}",
                    get_node_text(type_id, source),
                    get_node_text(name_id, source)
                ));
            }
            if let Some(type_id) = type_id {
                return Some(get_node_text(type_id, source).to_string());
            }
            return None;
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
    use crate::types::{EdgeKind, Language, NodeKind};

    #[test]
    fn dart_smoke_extraction() {
        let source = "import 'package:flutter/material.dart';\n\nclass Counter {\n  int value = 0;\n\n  void _bump() {\n    notify();\n  }\n}\n\nenum Phase { idle, busy }\n\nvoid notify() {}\n";
        let result = TreeSitterExtractor::new(
            "lib/counter.dart",
            source,
            Some(Language::Dart),
            Some(&DartExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let class = result.nodes.iter().find(|n| n.name == "Counter").unwrap();
        assert_eq!(class.kind, NodeKind::Class);

        let bump = result.nodes.iter().find(|n| n.name == "_bump").unwrap();
        assert_eq!(bump.kind, NodeKind::Method);
        // Dart `_` prefix → private
        assert_eq!(bump.visibility, Some(Visibility::Private));

        let phase = result.nodes.iter().find(|n| n.name == "Phase").unwrap();
        assert_eq!(phase.kind, NodeKind::Enum);
        let idle = result.nodes.iter().find(|n| n.name == "idle").unwrap();
        assert_eq!(idle.kind, NodeKind::EnumMember);

        let notify = result.nodes.iter().find(|n| n.name == "notify").unwrap();
        assert_eq!(notify.kind, NodeKind::Function);
        assert_eq!(notify.visibility, Some(Visibility::Public));

        // call recorded through extract_bare_call (identifier + selector)
        assert!(
            result
                .unresolved_references
                .iter()
                .any(|r| r.reference_name == "notify" && r.reference_kind == EdgeKind::Calls),
            "expected bare call reference to notify; got {:?}",
            result
                .unresolved_references
                .iter()
                .map(|r| &r.reference_name)
                .collect::<Vec<_>>()
        );

        let import = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Import)
            .expect("import node");
        assert_eq!(import.name, "package:flutter/material.dart");
    }
}

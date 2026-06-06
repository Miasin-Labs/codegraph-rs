//! Objective-C language extraction config.
//!
//! Ported from `src/extraction/languages/objc.ts`.

use super::{find_named_child, named_children};
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{
    ExtractorContext,
    ImportInfo,
    ImportOutcome,
    LanguageExtractor,
    NodeExtra,
    SyntaxNode,
};
use crate::types::NodeKind;

fn find_compound_statement<'t>(node: SyntaxNode<'t>) -> Option<SyntaxNode<'t>> {
    for i in 0..node.named_child_count() as u32 {
        if let Some(child) = node.named_child(i) {
            if child.kind() == "compound_statement" {
                return Some(child);
            }
        }
    }
    None
}

/// Build ObjC selector: `greet`, `doThing:`, or `doThing:with:`.
fn extract_objc_method_name(node: SyntaxNode<'_>, source: &str) -> Option<String> {
    if node.kind() != "method_definition" && node.kind() != "method_declaration" {
        return None;
    }

    let identifiers: Vec<SyntaxNode<'_>> = named_children(node)
        .into_iter()
        .filter(|c| c.kind() == "identifier")
        .collect();
    if identifiers.is_empty() {
        return None;
    }

    let has_parameters = named_children(node)
        .into_iter()
        .any(|c| c.kind() == "method_parameter");
    let first_identifier = identifiers[0];
    if !has_parameters {
        return Some(get_node_text(first_identifier, source).to_string());
    }

    Some(
        identifiers
            .iter()
            .map(|id| format!("{}:", get_node_text(*id, source)))
            .collect::<Vec<_>>()
            .join(""),
    )
}

fn extract_objc_property_name(node: SyntaxNode<'_>, source: &str) -> Option<String> {
    if node.kind() != "property_declaration" {
        return None;
    }

    let struct_decl = find_named_child(node, "struct_declaration")?;
    let struct_declarator = find_named_child(struct_decl, "struct_declarator")?;

    let mut current = Some(struct_declarator);
    while let Some(c) = current {
        let inner = get_child_by_field(c, "declarator").or_else(|| {
            named_children(c)
                .into_iter()
                .find(|n| n.kind() == "identifier" || n.kind() == "pointer_declarator")
        });
        let Some(inner) = inner else {
            break;
        };
        if inner.kind() == "identifier" {
            return Some(get_node_text(inner, source).to_string());
        }
        current = Some(inner);
    }

    None
}

pub struct ObjcExtractor;

impl LanguageExtractor for ObjcExtractor {
    fn function_types(&self) -> &[&str] {
        &["function_definition"]
    }
    fn class_types(&self) -> &[&str] {
        // Only @interface emits a class node; @implementation reuses it via visit_node.
        &["class_interface"]
    }
    fn method_types(&self) -> &[&str] {
        &["method_definition"]
    }
    fn interface_types(&self) -> &[&str] {
        &["protocol_declaration"]
    }
    fn interface_kind(&self) -> NodeKind {
        NodeKind::Protocol
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
        &["type_definition"]
    }
    fn import_types(&self) -> &[&str] {
        &["preproc_include"]
    }
    fn call_types(&self) -> &[&str] {
        &["call_expression", "message_expression"]
    }
    fn variable_types(&self) -> &[&str] {
        &["declaration"]
    }
    fn property_types(&self) -> &[&str] {
        &["property_declaration"]
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
        extract_objc_method_name(node, source)
    }

    fn extract_property_name(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        extract_objc_property_name(node, source)
    }

    fn resolve_body<'t>(&self, node: SyntaxNode<'t>, body_field: &str) -> Option<SyntaxNode<'t>> {
        if let Some(from_field) = get_child_by_field(node, body_field) {
            return Some(from_field);
        }
        find_compound_statement(node)
    }

    fn resolve_type_alias_kind(&self, node: SyntaxNode<'_>, _source: &str) -> Option<NodeKind> {
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

    fn is_static(&self, node: SyntaxNode<'_>, source: &str) -> Option<bool> {
        // TS: /^\s*\+/.test(node.text) — class methods start with `+`
        Some(get_node_text(node, source).trim_start().starts_with('+'))
    }

    fn visit_node(&self, node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
        if node.kind() != "class_implementation" {
            return false;
        }

        let Some(class_name_node) = find_named_child(node, "identifier") else {
            return true;
        };

        let class_name = get_node_text(class_name_node, ctx.source()).to_string();
        let existing_id = ctx
            .nodes()
            .iter()
            .find(|n| {
                n.name == class_name && n.file_path == ctx.file_path() && n.kind == NodeKind::Class
            })
            .map(|n| n.id.clone());
        let class_id = match existing_id {
            Some(id) => Some(id),
            None => ctx
                .create_node(NodeKind::Class, &class_name, node, NodeExtra::default())
                .map(|n| n.id),
        };
        let Some(class_id) = class_id else {
            return true;
        };

        ctx.push_scope(class_id);
        for i in 0..node.named_child_count() as u32 {
            if let Some(child) = node.named_child(i) {
                if child.kind() == "implementation_definition" {
                    for j in 0..child.named_child_count() as u32 {
                        if let Some(impl_child) = child.named_child(j) {
                            ctx.visit_node(impl_child);
                        }
                    }
                }
            }
        }
        ctx.pop_scope();
        true
    }

    fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
        let import_text = get_node_text(node, source).trim();
        if let Some(system_lib) = named_children(node)
            .into_iter()
            .find(|c| c.kind() == "system_lib_string")
        {
            let text = get_node_text(system_lib, source);
            let text = text.strip_prefix('<').unwrap_or(text);
            let text = text.strip_suffix('>').unwrap_or(text);
            return ImportOutcome::Info(ImportInfo::new(text, import_text));
        }
        if let Some(string_literal) = named_children(node)
            .into_iter()
            .find(|c| c.kind() == "string_literal")
        {
            if let Some(string_content) = find_named_child(string_literal, "string_content") {
                return ImportOutcome::Info(ImportInfo::new(
                    get_node_text(string_content, source),
                    import_text,
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
    fn objc_smoke_extraction() {
        let source = "#import <Foundation/Foundation.h>\n\n@interface Greeter : NSObject\n- (void)greet;\n@end\n\n@implementation Greeter\n- (void)greet {\n  NSLog(@\"hi\");\n}\n- (void)doThing:(int)a with:(int)b {\n}\n+ (instancetype)shared {\n  return nil;\n}\n@end\n";
        let result = TreeSitterExtractor::new(
            "src/Greeter.m",
            source,
            Some(Language::Objc),
            Some(&ObjcExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        // Exactly one class node — @implementation reuses the @interface node.
        let classes: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Class && n.name == "Greeter")
            .collect();
        assert_eq!(
            classes.len(),
            1,
            "implementation must reuse interface class"
        );

        // Multi-part selector name
        assert!(
            result.nodes.iter().any(|n| n.name == "doThing:with:"),
            "expected selector-style method name, got {:?}",
            result
                .nodes
                .iter()
                .map(|n| n.name.clone())
                .collect::<Vec<_>>()
        );

        // Class method (+) is static
        let shared = result.nodes.iter().find(|n| n.name == "shared").unwrap();
        assert_eq!(shared.is_static, Some(true));

        let import = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Import)
            .expect("import node");
        assert_eq!(import.name, "Foundation/Foundation.h");
    }

    #[test]
    fn objc_protocol_uses_protocol_kind() {
        let source = "@protocol Runnable\n- (void)run;\n@end\n";
        let result = TreeSitterExtractor::new(
            "src/Runnable.h",
            source,
            Some(Language::Objc),
            Some(&ObjcExtractor),
        )
        .extract();
        let proto = result
            .nodes
            .iter()
            .find(|n| n.name == "Runnable")
            .expect("protocol node");
        assert_eq!(proto.kind, NodeKind::Protocol);
    }
}

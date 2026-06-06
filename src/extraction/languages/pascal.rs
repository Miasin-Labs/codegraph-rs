//! Pascal (Delphi) language extraction config.
//!
//! Ported from `src/extraction/languages/pascal.ts`.

use super::find_named_child;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{LanguageExtractor, SyntaxNode};
use crate::types::Visibility;

pub struct PascalExtractor;

impl LanguageExtractor for PascalExtractor {
    fn function_types(&self) -> &[&str] {
        &["declProc"]
    }
    fn class_types(&self) -> &[&str] {
        &["declClass"]
    }
    fn method_types(&self) -> &[&str] {
        &["declProc"]
    }
    fn interface_types(&self) -> &[&str] {
        &["declIntf"]
    }
    fn struct_types(&self) -> &[&str] {
        &[]
    }
    fn enum_types(&self) -> &[&str] {
        &["declEnum"]
    }
    fn type_alias_types(&self) -> &[&str] {
        &["declType"]
    }
    fn import_types(&self) -> &[&str] {
        &["declUses"]
    }
    fn call_types(&self) -> &[&str] {
        &["exprCall"]
    }
    fn variable_types(&self) -> &[&str] {
        &["declField", "declConst"]
    }
    fn name_field(&self) -> &str {
        "name"
    }
    fn body_field(&self) -> &str {
        "body"
    }
    fn params_field(&self) -> &str {
        "args"
    }
    fn return_field(&self) -> Option<&str> {
        Some("type")
    }

    fn get_signature(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        let args = get_child_by_field(node, "args");
        let return_type = find_named_child(node, "typeref");
        if args.is_none() && return_type.is_none() {
            return None;
        }
        let mut sig = String::new();
        if let Some(args) = args {
            sig = get_node_text(args, source).to_string();
        }
        if let Some(rt) = return_type {
            sig.push_str(": ");
            sig.push_str(get_node_text(rt, source));
        }
        if sig.is_empty() { None } else { Some(sig) }
    }

    fn get_visibility(&self, node: SyntaxNode<'_>, _source: &str) -> Option<Visibility> {
        let mut current = node.parent();
        while let Some(c) = current {
            if c.kind() == "declSection" {
                for i in 0..c.child_count() as u32 {
                    if let Some(child) = c.child(i) {
                        match child.kind() {
                            "kPublic" | "kPublished" => return Some(Visibility::Public),
                            "kPrivate" => return Some(Visibility::Private),
                            "kProtected" => return Some(Visibility::Protected),
                            _ => {}
                        }
                    }
                }
            }
            current = c.parent();
        }
        None
    }

    fn is_exported(&self, _node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
        // In Pascal, symbols declared in the interface section are exported
        Some(false)
    }

    fn is_static(&self, node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
        for i in 0..node.child_count() as u32 {
            if let Some(child) = node.child(i) {
                if child.kind() == "kClass" {
                    return Some(true);
                }
            }
        }
        Some(false)
    }

    fn is_const(&self, node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
        Some(node.kind() == "declConst")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
    use crate::types::{Language, NodeKind};

    #[test]
    fn pascal_smoke_extraction() {
        let source = "unit Sample;\n\ninterface\n\ntype\n  TGreeter = class\n  public\n    procedure Greet;\n  end;\n\nimplementation\n\nprocedure TGreeter.Greet;\nbegin\n  WriteLn('hi');\nend;\n\nend.\n";
        let result = TreeSitterExtractor::new(
            "src/sample.pas",
            source,
            Some(Language::Pascal),
            Some(&PascalExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let class = result
            .nodes
            .iter()
            .find(|n| n.name.contains("TGreeter") && n.kind == NodeKind::Class);
        assert!(
            class.is_some(),
            "expected a class node, got {:?}",
            result
                .nodes
                .iter()
                .map(|n| (n.kind, n.name.clone()))
                .collect::<Vec<_>>()
        );

        // declProc appears for the procedure declaration and/or implementation
        let proc = result
            .nodes
            .iter()
            .find(|n| n.name.contains("Greet") && n.kind != NodeKind::File);
        assert!(proc.is_some(), "expected a Greet proc node");
    }
}

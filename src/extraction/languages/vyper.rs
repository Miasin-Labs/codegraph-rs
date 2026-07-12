//! Vyper language extraction config.

use super::named_children;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{
    ExtractorContext,
    ImportInfo,
    ImportOutcome,
    LanguageExtractor,
    NodeExtra,
    SyntaxNode,
};
use crate::types::{EdgeKind, NodeKind, UnresolvedReference, Visibility};

pub struct VyperExtractor;

impl LanguageExtractor for VyperExtractor {
    fn function_types(&self) -> &[&str] {
        &["function_definition"]
    }

    fn class_types(&self) -> &[&str] {
        &[]
    }

    fn method_types(&self) -> &[&str] {
        &["interface_sig"]
    }

    fn interface_types(&self) -> &[&str] {
        &["interface_definition"]
    }

    fn struct_types(&self) -> &[&str] {
        &["struct_definition"]
    }

    fn enum_types(&self) -> &[&str] {
        &["enum_definition"]
    }

    fn enum_member_types(&self) -> &[&str] {
        &["identifier"]
    }

    fn type_alias_types(&self) -> &[&str] {
        &["type_alias_statement"]
    }

    fn import_types(&self) -> &[&str] {
        &["import_statement", "import_from_statement"]
    }

    fn call_types(&self) -> &[&str] {
        &["call"]
    }

    fn variable_types(&self) -> &[&str] {
        &["assignment"]
    }

    fn field_types(&self) -> &[&str] {
        &["assignment"]
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

    fn resolve_name(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        (node.kind() == "type_alias_statement")
            .then(|| get_child_by_field(node, "left"))
            .flatten()
            .map(|name| get_node_text(name, source).to_string())
    }

    fn resolve_body<'tree>(
        &self,
        node: SyntaxNode<'tree>,
        _body_field: &str,
    ) -> Option<SyntaxNode<'tree>> {
        (node.kind() == "enum_definition")
            .then(|| get_child_by_field(node, "members"))
            .flatten()
    }

    fn get_signature(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        let parameters = get_child_by_field(node, "parameters")?;
        let mut signature = get_node_text(parameters, source).to_string();
        if let Some(return_type) = get_child_by_field(node, "return_type") {
            signature.push_str(" -> ");
            signature.push_str(get_node_text(return_type, source));
        }
        Some(signature)
    }

    fn get_visibility(&self, node: SyntaxNode<'_>, source: &str) -> Option<Visibility> {
        if node.kind() == "interface_sig" {
            return Some(Visibility::Public);
        }
        let parent = node
            .parent()
            .filter(|p| p.kind() == "decorated_definition")?;
        named_children(parent)
            .into_iter()
            .filter(|child| child.kind() == "decorator")
            .find_map(|decorator| match get_node_text(decorator, source) {
                text if text.contains("external") => Some(Visibility::Public),
                text if text.contains("internal") => Some(Visibility::Internal),
                _ => None,
            })
    }

    fn is_const(&self, node: SyntaxNode<'_>, source: &str) -> Option<bool> {
        Some(
            get_child_by_field(node, "type")
                .map(|kind| get_node_text(kind, source).trim().starts_with("constant("))
                .unwrap_or(false),
        )
    }

    fn extract_modifiers(&self, node: SyntaxNode<'_>, source: &str) -> Option<Vec<String>> {
        let parent = node
            .parent()
            .filter(|parent| parent.kind() == "decorated_definition")?;
        let modifiers: Vec<_> = named_children(parent)
            .into_iter()
            .filter(|child| child.kind() == "decorator")
            .map(|decorator| {
                get_node_text(decorator, source)
                    .trim()
                    .trim_start_matches('@')
                    .to_string()
            })
            .collect();
        (!modifiers.is_empty()).then_some(modifiers)
    }

    fn visit_node(&self, node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
        match node.kind() {
            "event_definition" => visit_event(node, ctx),
            "import_statement" => visit_imports(node, ctx),
            _ => false,
        }
    }

    fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
        let module = if node.kind() == "import_from_statement" {
            get_child_by_field(node, "module_name")
        } else {
            get_child_by_field(node, "name").and_then(|name| {
                if name.kind() == "aliased_import" {
                    get_child_by_field(name, "name")
                } else {
                    Some(name)
                }
            })
        };
        let Some(module) = module else {
            return ImportOutcome::Declined;
        };
        ImportOutcome::Info(ImportInfo::new(
            get_node_text(module, source),
            get_node_text(node, source).trim(),
        ))
    }
}

fn visit_event(node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
    let Some(name_node) = get_child_by_field(node, "name") else {
        return true;
    };
    let (name, signature) = {
        let source = ctx.source();
        (
            get_node_text(name_node, source).to_string(),
            get_node_text(node, source)
                .trim()
                .chars()
                .take(200)
                .collect(),
        )
    };
    ctx.create_node(
        NodeKind::Field,
        &name,
        node,
        NodeExtra {
            signature: Some(signature),
            ..Default::default()
        },
    );
    true
}

fn visit_imports(node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
    let signature = get_node_text(node, ctx.source()).trim().to_string();
    let parent_id = ctx.node_stack().last().cloned();
    let mut handled = false;
    for import in named_children(node) {
        let module = if import.kind() == "aliased_import" {
            get_child_by_field(import, "name")
        } else if import.kind() == "dotted_name" {
            Some(import)
        } else {
            None
        };
        let Some(module) = module else { continue };
        let module_name = get_node_text(module, ctx.source()).to_string();
        ctx.create_node(
            NodeKind::Import,
            &module_name,
            import,
            NodeExtra {
                signature: Some(signature.clone()),
                ..Default::default()
            },
        );
        if let Some(from_node_id) = parent_id.clone() {
            ctx.add_unresolved_reference(UnresolvedReference {
                from_node_id,
                reference_name: module_name,
                reference_kind: EdgeKind::Imports,
                line: import.start_position().row as u32 + 1,
                column: import.start_position().column as u32,
                file_path: None,
                language: None,
                candidates: None,
                metadata: None,
            });
        }
        handled = true;
    }
    handled
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
    use crate::types::{EdgeKind, Language};

    #[test]
    fn vyper_smoke_extraction() {
        let source = r#"import foo as bar, baz
from pkg import Baz as Qux

event Transfer:
    sender: indexed(address)

struct User:
    balance: uint256

interface Token:
    def transfer(to: address, amount: uint256) -> bool: nonpayable
    def balanceOf(owner: address) -> uint256: view

counter: public(uint256)
owner: User
LIMIT: constant(uint256) = 100

@internal
def _amount() -> uint256:
    return LIMIT

@external
def ping(token: Token, to: address):
    ok: bool = extcall token.transfer(to, _amount())
    balance: uint256 = staticcall token.balanceOf(to)
    log Transfer(to, self, balance)
"#;
        let result = TreeSitterExtractor::new(
            "contracts/token.vy",
            source,
            Some(Language::Vyper),
            Some(&VyperExtractor),
        )
        .extract();

        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.nodes.len(), 15, "nodes: {:#?}", result.nodes);
        assert_eq!(
            result.unresolved_references.len(),
            9,
            "references: {:#?}",
            result.unresolved_references
        );

        for name in [
            "foo",
            "baz",
            "pkg",
            "Transfer",
            "User",
            "balance",
            "Token",
            "transfer",
            "balanceOf",
            "counter",
            "owner",
            "LIMIT",
            "_amount",
            "ping",
        ] {
            assert!(result.nodes.iter().any(|node| node.name == name), "{name}");
        }

        let mut calls: Vec<&str> = result
            .unresolved_references
            .iter()
            .filter(|reference| reference.reference_kind == EdgeKind::Calls)
            .map(|reference| reference.reference_name.as_str())
            .collect();
        calls.sort_unstable();
        assert_eq!(
            calls,
            ["Transfer", "_amount", "token.balanceOf", "token.transfer"]
        );
        assert!(
            !result
                .unresolved_references
                .iter()
                .any(|reference| reference.reference_kind == EdgeKind::Decorates)
        );
        assert!(result.unresolved_references.iter().any(|reference| {
            reference.reference_kind == EdgeKind::References && reference.reference_name == "Token"
        }));
        assert!(result.unresolved_references.iter().any(|reference| {
            reference.reference_kind == EdgeKind::References && reference.reference_name == "User"
        }));
        for (name, modifier) in [("_amount", "internal"), ("ping", "external")] {
            let node = result
                .nodes
                .iter()
                .find(|node| node.name == name)
                .unwrap_or_else(|| panic!("missing {name}"));
            assert!(
                node.decorators
                    .as_ref()
                    .is_some_and(|decorators| decorators.iter().any(|item| item == modifier)),
                "missing {modifier} modifier on {name}"
            );
        }
    }
}

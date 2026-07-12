//! Sway language extraction config.

use super::named_children;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{
    ClassLikeKind,
    ExtractorContext,
    ImportInfo,
    ImportOutcome,
    LanguageExtractor,
    NodeExtra,
    SyntaxNode,
};
use crate::types::{NodeKind, Visibility};

pub struct SwayExtractor;

impl LanguageExtractor for SwayExtractor {
    fn function_types(&self) -> &[&str] {
        &[]
    }

    fn class_types(&self) -> &[&str] {
        &["trait_item"]
    }

    fn method_types(&self) -> &[&str] {
        &["function_item", "function_signature_item"]
    }

    fn interface_types(&self) -> &[&str] {
        &["abi_item"]
    }

    fn struct_types(&self) -> &[&str] {
        &["struct_item"]
    }

    fn enum_types(&self) -> &[&str] {
        &["enum_item"]
    }

    fn enum_member_types(&self) -> &[&str] {
        &["enum_variant"]
    }

    fn type_alias_types(&self) -> &[&str] {
        &["type_item"]
    }

    fn import_types(&self) -> &[&str] {
        &["use_declaration"]
    }

    fn call_types(&self) -> &[&str] {
        &["call_expression", "abi_call_expression"]
    }

    fn variable_types(&self) -> &[&str] {
        &["let_declaration", "const_item"]
    }

    fn field_types(&self) -> &[&str] {
        &["field_declaration"]
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

    fn module_is_class_like(&self) -> bool {
        false
    }

    fn classify_class_node(&self, node: SyntaxNode<'_>, _source: &str) -> ClassLikeKind {
        if node.kind() == "trait_item" {
            ClassLikeKind::Trait
        } else {
            ClassLikeKind::Class
        }
    }

    fn struct_is_definition_without_body(&self) -> bool {
        true
    }

    fn extract_member_variables(&self) -> bool {
        true
    }

    fn visit_node(&self, node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
        if node.kind() != "storage_content" {
            return false;
        }

        let Some(field) = named_children(node)
            .into_iter()
            .find(|child| child.kind() == "field_declaration")
        else {
            return false;
        };
        let Some(name_node) = get_child_by_field(field, "name") else {
            return false;
        };
        let name = get_node_text(name_node, ctx.source()).to_string();
        let signature = get_node_text(node, ctx.source()).trim().to_string();
        let kind = if has_ancestor(node, "configurable_item") {
            NodeKind::Constant
        } else {
            NodeKind::Variable
        };
        ctx.create_node(
            kind,
            &name,
            node,
            NodeExtra {
                signature: Some(signature),
                ..Default::default()
            },
        );

        // Keep walking so calls in the initializer are still recorded.
        false
    }

    fn get_signature(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        let params = get_child_by_field(node, "parameters")?;
        let mut signature = get_node_text(params, source).to_string();
        if let Some(return_type) = get_child_by_field(node, "return_type") {
            signature.push_str(" -> ");
            signature.push_str(get_node_text(return_type, source));
        }
        Some(signature)
    }

    fn get_visibility(&self, node: SyntaxNode<'_>, source: &str) -> Option<Visibility> {
        if has_ancestor(node, "abi_item") {
            return Some(Visibility::Public);
        }
        for child in named_children(node) {
            if child.kind() == "visibility_modifier" {
                return Some(if get_node_text(child, source).contains("pub") {
                    Visibility::Public
                } else {
                    Visibility::Private
                });
            }
        }
        Some(Visibility::Private)
    }

    fn is_const(&self, node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
        Some(node.kind() == "const_item")
    }

    fn get_receiver_type(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        let mut parent = node.parent();
        while let Some(current) = parent {
            if current.kind() == "impl_item" {
                let type_node = get_child_by_field(current, "type")?;
                return Some(get_node_text(type_node, source).to_string());
            }
            parent = current.parent();
        }
        None
    }

    fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
        let Some(argument) = get_child_by_field(node, "argument") else {
            return ImportOutcome::Declined;
        };
        let path = get_node_text(argument, source).trim_start_matches("::");
        let Some(root) = path.split("::").next().filter(|root| !root.is_empty()) else {
            return ImportOutcome::Declined;
        };
        ImportOutcome::Info(ImportInfo::new(root, get_node_text(node, source).trim()))
    }
}

fn has_ancestor(node: SyntaxNode<'_>, kind: &str) -> bool {
    let mut current = Some(node);
    while let Some(node) = current {
        if node.kind() == kind {
            return true;
        }
        current = node.parent();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
    use crate::types::Language;

    #[test]
    fn sway_smoke_extraction() {
        let source = r#"
contract;

use std::auth::msg_sender;

trait Reset {
    fn reset();
}

abi Counter {
    #[storage(read, write)]
    fn increment(amount: u64) -> u64;
    fn owner() -> Identity;
}

storage {
    count: u64 = 0,
}

pub struct Transfer {
    recipient: Identity,
    amount: u64,
}

enum Status {
    Active: (),
    Paused: (),
}

impl Counter for Contract {
    #[storage(read, write)]
    fn increment(amount: u64) -> u64 {
        let sender = msg_sender().unwrap();
        storage.count.write(storage.count.read() + amount);
        notify(sender);
        storage.count.read()
    }

    fn owner() -> Identity {
        msg_sender().unwrap()
    }
}

fn notify(sender: Identity) {
    log(sender);
}

fn call_counter(counter: Counter, amount: u64) {
    counter.increment { coins: amount }(amount);
}
"#;
        let result = TreeSitterExtractor::new(
            "src/main.sw",
            source,
            Some(Language::Sway),
            Some(&SwayExtractor),
        )
        .extract();

        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.nodes.len(), 18, "nodes: {:#?}", result.nodes);
        assert_eq!(
            result.unresolved_references.len(),
            15,
            "references: {:#?}",
            result.unresolved_references
        );

        let node = |name: &str, kind: NodeKind| {
            result
                .nodes
                .iter()
                .find(|node| node.name == name && node.kind == kind)
                .unwrap_or_else(|| panic!("missing {kind:?} {name}"))
        };
        node("Counter", NodeKind::Interface);
        node("Reset", NodeKind::Trait);
        node("count", NodeKind::Variable);
        node("Transfer", NodeKind::Struct);
        node("Status", NodeKind::Enum);
        node("Active", NodeKind::EnumMember);
        node("notify", NodeKind::Function);

        let increment = result
            .nodes
            .iter()
            .find(|node| node.qualified_name == "Contract::increment")
            .expect("receiver-qualified contract method");
        assert_eq!(increment.kind, NodeKind::Method);
        assert!(
            result
                .unresolved_references
                .iter()
                .any(|reference| reference.reference_name == "counter.increment")
        );
    }
}

//! Fe smart-contract language extraction configuration.

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
use crate::types::{NodeKind, Visibility};

pub struct FeExtractor;

impl LanguageExtractor for FeExtractor {
    fn function_types(&self) -> &[&str] {
        &["function_definition"]
    }

    fn class_types(&self) -> &[&str] {
        &["contract_definition"]
    }

    fn method_types(&self) -> &[&str] {
        &["function_definition", "contract_init", "recv_arm"]
    }

    fn interface_types(&self) -> &[&str] {
        &["trait_definition"]
    }

    fn struct_types(&self) -> &[&str] {
        &["struct_definition"]
    }

    fn enum_types(&self) -> &[&str] {
        &["enum_definition", "msg_definition"]
    }

    fn enum_member_types(&self) -> &[&str] {
        // These payload shapes do not expose a generic `body` field, so the
        // custom visitor creates the member and its fields together.
        &[]
    }

    fn type_alias_types(&self) -> &[&str] {
        &["type_alias", "trait_type_item"]
    }

    fn import_types(&self) -> &[&str] {
        &["use_statement"]
    }

    fn call_types(&self) -> &[&str] {
        &[]
    }

    fn variable_types(&self) -> &[&str] {
        &[
            "let_statement",
            "const_definition",
            "trait_const_item",
            "impl_const_item",
        ]
    }

    fn field_types(&self) -> &[&str] {
        &["record_field_def"]
    }

    fn name_field(&self) -> &str {
        "name"
    }

    fn body_field(&self) -> &str {
        "body"
    }

    fn params_field(&self) -> &str {
        ""
    }

    fn return_field(&self) -> Option<&str> {
        Some("return_type")
    }

    fn interface_kind(&self) -> NodeKind {
        NodeKind::Trait
    }

    fn module_is_class_like(&self) -> bool {
        false
    }

    fn extract_member_variables(&self) -> bool {
        true
    }

    fn resolve_name(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        match node.kind() {
            "contract_init" => Some("init".to_string()),
            "recv_arm" => {
                let pattern = find_named_child(node, "recv_arm_pattern")?;
                if let Some(name) = get_child_by_field(pattern, "name") {
                    return Some(get_node_text(name, source).to_string());
                }
                Some(
                    get_node_text(pattern, source)
                        .split('{')
                        .next()
                        .unwrap_or("_")
                        .trim()
                        .to_string(),
                )
            }
            _ => None,
        }
    }

    fn resolve_body<'t>(&self, node: SyntaxNode<'t>, _body_field: &str) -> Option<SyntaxNode<'t>> {
        (node.kind() == "msg_definition").then_some(node)
    }

    fn get_signature(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        let end = get_child_by_field(node, "body")
            .map(|body| body.start_byte())
            .unwrap_or_else(|| node.end_byte());
        source
            .get(node.start_byte()..end)
            .map(str::trim)
            .filter(|signature| !signature.is_empty())
            .map(str::to_string)
    }

    fn get_visibility(&self, node: SyntaxNode<'_>, source: &str) -> Option<Visibility> {
        let visibility = named_children(node)
            .into_iter()
            .find(|child| child.kind() == "visibility");
        Some(
            match visibility.map(|node| get_node_text(node, source).trim()) {
                Some("pub") => Visibility::Public,
                Some(_) => Visibility::Internal,
                None => Visibility::Private,
            },
        )
    }

    fn is_const(&self, node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
        Some(matches!(
            node.kind(),
            "const_definition" | "trait_const_item" | "impl_const_item"
        ))
    }

    fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
        let signature = get_node_text(node, source).trim();
        let Some(use_tree) = find_named_child(node, "use_tree") else {
            return ImportOutcome::Declined;
        };
        let Some(root) = named_children(use_tree)
            .into_iter()
            .find(|child| child.kind() == "use_path_segment")
        else {
            return ImportOutcome::Declined;
        };
        ImportOutcome::Info(ImportInfo::new(get_node_text(root, source), signature))
    }

    fn extract_bare_call(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        match node.kind() {
            "call_expression" | "macro_call_expression" => {
                let function = get_child_by_field(node, "function")?;
                let raw = get_node_text(function, source);
                if function.kind() != "field_expression" {
                    return Some(normalize_call_name(raw));
                }
                let method = get_node_text(get_child_by_field(function, "field")?, source);
                let receiver = raw.strip_suffix(method)?.trim_end_matches('.');
                if matches!(receiver, "self" | "Self" | "super") {
                    Some(normalize_call_name(method))
                } else {
                    Some(normalize_call_name(&format!("{receiver}.{method}")))
                }
            }
            "method_call_expression" => {
                let method = get_node_text(get_child_by_field(node, "method")?, source);
                let receiver = get_child_by_field(node, "value")?;
                let receiver_name = get_node_text(receiver, source);
                if matches!(receiver_name, "self" | "Self" | "super") {
                    Some(normalize_call_name(method))
                } else if receiver.kind() == "identifier" {
                    Some(normalize_call_name(&format!("{receiver_name}.{method}")))
                } else {
                    Some(normalize_call_name(method))
                }
            }
            _ => None,
        }
    }

    fn get_receiver_type(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        let mut parent = node.parent();
        while let Some(candidate) = parent {
            if matches!(candidate.kind(), "impl_block" | "impl_trait") {
                let target = get_child_by_field(candidate, "type")?;
                return normalize_type_name(get_node_text(target, source));
            }
            parent = candidate.parent();
        }
        None
    }

    fn visit_node(&self, node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
        match node.kind() {
            "mod_definition" => {
                let Some(name_node) = get_child_by_field(node, "name") else {
                    return false;
                };
                let name = get_node_text(name_node, ctx.source()).to_string();
                let Some(module) = ctx.create_node(
                    NodeKind::Module,
                    &name,
                    node,
                    NodeExtra {
                        visibility: self.get_visibility(node, ctx.source()),
                        ..Default::default()
                    },
                ) else {
                    return true;
                };
                ctx.push_scope(module.id);
                for child in named_children(node) {
                    if child != name_node {
                        ctx.visit_node(child);
                    }
                }
                ctx.pop_scope();
                true
            }
            "impl_block" | "impl_trait" => {
                let Some(body) = get_child_by_field(node, "body") else {
                    return true;
                };
                for child in named_children(body) {
                    ctx.visit_node(child);
                }
                true
            }
            "variant_def" | "msg_variant" => visit_variant(node, ctx),
            _ => false,
        }
    }
}

fn normalize_call_name(raw: &str) -> String {
    let mut normalized = String::with_capacity(raw.len());
    let mut generic_depth = 0u32;
    for ch in raw.trim().trim_end_matches('!').chars() {
        match ch {
            '<' => generic_depth += 1,
            '>' if generic_depth > 0 => generic_depth -= 1,
            _ if generic_depth == 0 => normalized.push(ch),
            _ => {}
        }
    }
    normalized.trim_end_matches("::").to_string()
}

fn normalize_type_name(raw: &str) -> Option<String> {
    let normalized = normalize_call_name(raw);
    let name = normalized.trim().trim_start_matches('&').trim();
    (!name.is_empty()).then(|| name.to_string())
}

fn visit_variant(node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
    let Some(name_node) = get_child_by_field(node, "name") else {
        return true;
    };
    let name = get_node_text(name_node, ctx.source()).to_string();
    let Some(member) = ctx.create_node(NodeKind::EnumMember, &name, node, NodeExtra::default())
    else {
        return true;
    };

    let payload = named_children(node).into_iter().find(|child| {
        matches!(
            child.kind(),
            "record_field_def_list" | "msg_variant_params" | "tuple_type"
        )
    });
    let Some(payload) = payload else {
        return true;
    };

    ctx.push_scope(member.id);
    if payload.kind() == "tuple_type" {
        for (index, field) in named_children(payload).into_iter().enumerate() {
            let type_name = get_node_text(field, ctx.source()).to_string();
            let name = index.to_string();
            ctx.create_node(
                NodeKind::Field,
                &name,
                field,
                NodeExtra {
                    signature: Some(format!("{name}: {type_name}")),
                    ..Default::default()
                },
            );
        }
    } else {
        for field in named_children(payload) {
            ctx.visit_node(field);
        }
    }
    ctx.pop_scope();
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
    use crate::types::{EdgeKind, Language};

    #[test]
    fn fe_smoke_extraction() {
        let source = r#"use std::math::max

pub struct Account {
    owner: address,
    mut balance: u256,
}

pub enum Action {
    Deposit(u256),
    Withdraw { amount: u256 },
}

pub fn capped(service: Service, value: u256, limit: u256) -> u256 {
    service.run::<u256>(value)
    return max(value, limit)
}

pub contract Vault {
    mut balance: u256

    init(initial: u256) {
        self.balance = initial
    }

    recv {
        Deposit { amount } {
            self.credit(amount)
        }
        Balance -> u256 {
            return self.balance
        }
    }
}
"#;
        let result = TreeSitterExtractor::new(
            "src/vault.fe",
            source,
            Some(Language::Fe),
            Some(&FeExtractor),
        )
        .extract();

        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.nodes.len(), 16);
        assert_eq!(result.unresolved_references.len(), 5);

        for (kind, name) in [
            (NodeKind::Struct, "Account"),
            (NodeKind::Enum, "Action"),
            (NodeKind::Function, "capped"),
            (NodeKind::Class, "Vault"),
            (NodeKind::Method, "init"),
            (NodeKind::Method, "Deposit"),
            (NodeKind::Method, "Balance"),
        ] {
            assert!(
                result
                    .nodes
                    .iter()
                    .any(|node| node.kind == kind && node.name == name),
                "missing {kind:?} {name}"
            );
        }

        for qualified_name in ["Action::Deposit::0", "Action::Withdraw::amount"] {
            assert!(
                result.nodes.iter().any(|node| {
                    node.kind == NodeKind::Field && node.qualified_name == qualified_name
                }),
                "missing payload field {qualified_name}"
            );
        }

        for (kind, name) in [
            (EdgeKind::Imports, "std"),
            (EdgeKind::Calls, "service.run"),
            (EdgeKind::Calls, "max"),
            (EdgeKind::Calls, "credit"),
        ] {
            assert!(
                result.unresolved_references.iter().any(|reference| {
                    reference.reference_kind == kind && reference.reference_name == name
                }),
                "missing {kind:?} reference to {name}"
            );
        }
    }

    #[test]
    fn fe_impl_calls_and_trait_types_are_normalized() {
        let source = r#"impl Worker {
    pub fn run(value: u256) {
        audit<u256>(value)
        verify!(value)
    }
}

pub struct Worker {}

pub trait Config {
    type Value
}
"#;
        let result = TreeSitterExtractor::new(
            "src/worker.fe",
            source,
            Some(Language::Fe),
            Some(&FeExtractor),
        )
        .extract();

        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let method = result
            .nodes
            .iter()
            .find(|node| node.name == "run" && node.kind == NodeKind::Method)
            .expect("impl method");
        assert_eq!(method.qualified_name, "Worker::run");
        assert!(
            result
                .nodes
                .iter()
                .any(|node| node.name == "Value" && node.kind == NodeKind::TypeAlias)
        );
        for call in ["audit", "verify"] {
            assert!(
                result.unresolved_references.iter().any(|reference| {
                    reference.reference_kind == EdgeKind::Calls && reference.reference_name == call
                }),
                "missing normalized call {call}"
            );
        }
    }
}

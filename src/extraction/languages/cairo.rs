//! Cairo language extraction config.

use super::{find_named_child, named_children};
use crate::extraction::languages::RustExtractor;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{
    ExtractorContext,
    ImportInfo,
    ImportOutcome,
    LanguageExtractor,
    NodeExtra,
    SyntaxNode,
    normalize_return_type_text,
};
use crate::types::{NodeKind, Visibility};

pub struct CairoExtractor;

fn function_header(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    (node.kind() == "function")
        .then_some(node)
        .or_else(|| find_named_child(node, "function"))
}

fn enum_variant_name(node: SyntaxNode<'_>, source: &str) -> Option<String> {
    let variant = get_child_by_field(node, "variant")?;
    let name = if variant.kind() == "field_declaration" {
        get_child_by_field(variant, "name")?
    } else {
        variant
    };
    Some(get_node_text(name, source).to_string())
}

impl LanguageExtractor for CairoExtractor {
    fn function_types(&self) -> &[&str] {
        &[
            "function_item",
            "function_signature_item",
            "external_function_item",
        ]
    }

    fn class_types(&self) -> &[&str] {
        &[]
    }

    fn method_types(&self) -> &[&str] {
        &["function_item", "function_signature_item"]
    }

    fn interface_types(&self) -> &[&str] {
        &["trait_item"]
    }

    fn struct_types(&self) -> &[&str] {
        &["struct_item"]
    }

    fn enum_types(&self) -> &[&str] {
        &["enum_item"]
    }

    fn enum_member_types(&self) -> &[&str] {
        // Cairo payload variants nest their name in a field_declaration, so
        // visit_node handles both payload and unit variants consistently.
        &[]
    }

    fn type_alias_types(&self) -> &[&str] {
        &["type_item", "associated_type"]
    }

    fn import_types(&self) -> &[&str] {
        &["use_declaration"]
    }

    fn call_types(&self) -> &[&str] {
        &["call_expression"]
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
            "function_item" | "function_signature_item" | "external_function_item" => {
                let header = function_header(node)?;
                let name = get_child_by_field(header, "name")?;
                Some(get_node_text(name, source).to_string())
            }
            "enum_variant" => enum_variant_name(node, source),
            _ => None,
        }
    }

    fn get_signature(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        let header = function_header(node)?;
        let params = get_child_by_field(header, "parameters")?;
        let mut signature = get_node_text(params, source).to_string();
        if let Some(return_type) = get_child_by_field(header, "return_type") {
            signature.push_str(" -> ");
            signature.push_str(get_node_text(return_type, source));
        }
        Some(signature)
    }

    fn get_return_type(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        let header = function_header(node)?;
        let return_type = get_child_by_field(header, "return_type")?;
        normalize_return_type_text(get_node_text(return_type, source))
    }

    fn get_visibility(&self, node: SyntaxNode<'_>, source: &str) -> Option<Visibility> {
        if named_children(node).into_iter().any(|child| {
            child.kind() == "visibility_modifier" && get_node_text(child, source).contains("pub")
        }) {
            return Some(Visibility::Public);
        }

        let mut previous = node.prev_named_sibling();
        while let Some(attribute) = previous {
            if attribute.kind() != "attribute_item" {
                break;
            }
            let text = get_node_text(attribute, source);
            if text.starts_with("#[external")
                || text.starts_with("#[constructor")
                || text.starts_with("#[l1_handler")
            {
                return Some(Visibility::Public);
            }
            previous = attribute.prev_named_sibling();
        }
        Some(Visibility::Private)
    }

    fn is_const(&self, node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
        Some(node.kind() == "const_item")
    }

    fn get_receiver_type(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        let mut parent = node.parent();
        while let Some(candidate) = parent {
            if candidate.kind() == "impl_item" {
                return named_children(candidate)
                    .into_iter()
                    .find(|child| child.kind() == "identifier")
                    .map(|name| get_node_text(name, source).to_string());
            }
            parent = candidate.parent();
        }
        None
    }

    fn visit_node(&self, node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
        match node.kind() {
            "enum_variant" => {
                if let Some(name) = enum_variant_name(node, ctx.source()) {
                    ctx.create_node(NodeKind::EnumMember, &name, node, NodeExtra::default());
                }
                true
            }
            "impl_item" => {
                if let Some(body) = get_child_by_field(node, "body") {
                    for child in named_children(body) {
                        ctx.visit_node(child);
                    }
                }
                true
            }
            "mod_item" => RustExtractor.visit_node(node, ctx),
            _ => false,
        }
    }

    fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
        if get_child_by_field(node, "argument").is_some_and(|argument| argument.kind() == "super") {
            return ImportOutcome::Info(ImportInfo::new(
                "super",
                get_node_text(node, source).trim(),
            ));
        }
        RustExtractor.extract_import(node, source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
    use crate::types::{EdgeKind, Language};

    #[test]
    fn cairo_starknet_smoke_extraction() {
        let source = r#"#[starknet::contract]
mod Counter {
    use starknet::ContractAddress;
    use super;

    #[storage]
    struct Storage { value: u128, owner: ContractAddress }

    #[event]
    enum Event { ValueChanged: ValueChanged }

    struct ValueChanged { old_value: u128, new_value: u128 }

    #[constructor]
    fn constructor(ref self: ContractState, owner: ContractAddress) {
        self.owner.write(owner);
    }

    #[external(v0)]
    fn increment(ref self: ContractState, amount: u128) -> u128 {
        let old = self.value.read();
        let updated = old + amount;
        self.value.write(updated);
        self.emit(ValueChanged { old_value: old, new_value: updated });
        updated
    }
}
"#;
        let result = TreeSitterExtractor::new(
            "src/counter.cairo",
            source,
            Some(Language::Cairo),
            Some(&CairoExtractor),
        )
        .extract();

        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.nodes.len(), 14);
        assert_eq!(result.unresolved_references.len(), 9);

        let find = |name: &str, kind: NodeKind| {
            result
                .nodes
                .iter()
                .find(|node| node.name == name && node.kind == kind)
        };
        assert!(find("Counter", NodeKind::Module).is_some());
        assert!(find("Storage", NodeKind::Struct).is_some());
        assert!(find("Event", NodeKind::Enum).is_some());
        assert!(find("ValueChanged", NodeKind::EnumMember).is_some());
        assert!(find("ValueChanged", NodeKind::Struct).is_some());
        let increment = find("increment", NodeKind::Function).expect("increment function");
        assert_eq!(increment.qualified_name, "Counter::increment");
        assert_eq!(increment.visibility, Some(Visibility::Public));
        assert_eq!(
            increment.signature.as_deref(),
            Some("(ref self: ContractState, amount: u128) -> u128")
        );

        let mut calls: Vec<_> = result
            .unresolved_references
            .iter()
            .filter(|reference| reference.reference_kind == EdgeKind::Calls)
            .map(|reference| reference.reference_name.as_str())
            .collect();
        calls.sort_unstable();
        assert_eq!(calls, ["emit", "read", "write", "write"]);
        assert!(result.unresolved_references.iter().any(|reference| {
            reference.reference_kind == EdgeKind::Imports && reference.reference_name == "super"
        }));
    }

    #[test]
    fn cairo_impl_methods_skip_rust_fallback() {
        let source = r#"trait Worker {
    fn run(self: felt252);
}

impl WorkerImpl of Worker {
    fn run(self: felt252) {
        ping();
    }
}
"#;
        let result = TreeSitterExtractor::new(
            "src/worker.cairo",
            source,
            Some(Language::Cairo),
            Some(&CairoExtractor),
        )
        .extract();

        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let method = result
            .nodes
            .iter()
            .find(|node| node.name == "run" && node.qualified_name == "WorkerImpl::run")
            .expect("receiver-qualified impl method");
        assert_eq!(method.kind, NodeKind::Method);
    }
}

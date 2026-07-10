//! Solidity language extraction config.
//!
//! Ported from `src/extraction/languages/solidity.ts`.

use std::collections::VecDeque;

use super::named_children;
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
use crate::types::{EdgeKind, NodeKind, UnresolvedReference, Visibility};

fn last_identifier_descendant<'tree>(node: SyntaxNode<'tree>) -> Option<SyntaxNode<'tree>> {
    let mut queue = VecDeque::from([node]);
    let mut last = None;
    while let Some(current) = queue.pop_front() {
        if current.kind() == "identifier"
            && last.is_none_or(|identifier: SyntaxNode<'_>| {
                current.start_byte() >= identifier.start_byte()
            })
        {
            last = Some(current);
        }
        queue.extend(named_children(current));
    }
    last
}

fn first_descendant<'tree>(node: SyntaxNode<'tree>, kind: &str) -> Option<SyntaxNode<'tree>> {
    let mut queue = VecDeque::from([node]);
    while let Some(current) = queue.pop_front() {
        if current.kind() == kind {
            return Some(current);
        }
        queue.extend(named_children(current));
    }
    None
}

fn inheritance_ancestors(node: SyntaxNode<'_>, source: &str) -> Vec<String> {
    named_children(node)
        .into_iter()
        .filter(|child| child.kind() == "inheritance_specifier")
        .filter_map(|specifier| get_child_by_field(specifier, "ancestor"))
        .filter_map(last_identifier_descendant)
        .map(|identifier| get_node_text(identifier, source).to_string())
        .collect()
}

fn fallback_receive_name(node: SyntaxNode<'_>, source: &str) -> String {
    for index in 0..node.child_count() as u32 {
        let Some(child) = node.child(index) else {
            continue;
        };
        let text = get_node_text(child, source);
        if matches!(text, "fallback" | "receive") {
            return text.to_string();
        }
    }
    "fallback".to_string()
}

fn declaration_signature(node: SyntaxNode<'_>, source: &str) -> String {
    let mut parameters = Vec::new();
    let mut return_type = None;
    let mut visibility = None;
    let mut mutability = None;

    for index in 0..node.named_child_count() as u32 {
        let Some(child) = node.named_child(index) else {
            continue;
        };
        let field_name = node.field_name_for_named_child(index);
        if child.kind() == "parameter" && field_name != Some("return_type") {
            parameters.push(get_node_text(child, source).to_string());
        } else if child.kind() == "return_type_definition" || field_name == Some("return_type") {
            return_type = Some(child);
        } else if child.kind() == "visibility" {
            visibility = Some(child);
        } else if child.kind() == "state_mutability" {
            mutability = Some(child);
        }
    }

    let mut parts = vec![format!("({})", parameters.join(", "))];
    for node in [visibility, mutability, return_type].into_iter().flatten() {
        parts.push(get_node_text(node, source).to_string());
    }
    parts.join(" ")
}

fn solidity_return_type(node: SyntaxNode<'_>, source: &str) -> Option<String> {
    let returns = get_child_by_field(node, "return_type")?;
    let mut parameters = named_children(returns)
        .into_iter()
        .filter(|child| child.kind() == "parameter");
    let parameter = parameters.next()?;
    if parameters.next().is_some() {
        return None;
    }
    let type_node = get_child_by_field(parameter, "type")?;
    normalize_return_type_text(get_node_text(type_node, source))
}

fn strip_string_quotes(value: &str) -> &str {
    let value = value
        .strip_prefix('"')
        .or_else(|| value.strip_prefix('\''))
        .unwrap_or(value);
    value
        .strip_suffix('"')
        .or_else(|| value.strip_suffix('\''))
        .unwrap_or(value)
}

pub struct SolidityExtractor;

impl LanguageExtractor for SolidityExtractor {
    fn function_types(&self) -> &[&str] {
        &["function_definition", "modifier_definition"]
    }

    fn class_types(&self) -> &[&str] {
        &["contract_declaration", "library_declaration"]
    }

    fn method_types(&self) -> &[&str] {
        &[
            "function_definition",
            "modifier_definition",
            "constructor_definition",
            "fallback_receive_definition",
        ]
    }

    fn interface_types(&self) -> &[&str] {
        &["interface_declaration"]
    }

    fn struct_types(&self) -> &[&str] {
        &["struct_declaration"]
    }

    fn enum_types(&self) -> &[&str] {
        &["enum_declaration"]
    }

    fn enum_member_types(&self) -> &[&str] {
        &[]
    }

    fn type_alias_types(&self) -> &[&str] {
        &["user_defined_type_definition"]
    }

    fn import_types(&self) -> &[&str] {
        &["import_directive"]
    }

    fn call_types(&self) -> &[&str] {
        &[
            "call_expression",
            "emit_statement",
            "revert_statement",
            "modifier_invocation",
        ]
    }

    fn variable_types(&self) -> &[&str] {
        &[
            "state_variable_declaration",
            "constant_variable_declaration",
        ]
    }

    fn field_types(&self) -> &[&str] {
        &["state_variable_declaration", "struct_member"]
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

    fn get_return_type(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        solidity_return_type(node, source)
    }

    fn resolve_name(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        match node.kind() {
            "constructor_definition" => Some("constructor".to_string()),
            "fallback_receive_definition" => Some(fallback_receive_name(node, source)),
            _ => None,
        }
    }

    fn get_signature(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        Some(declaration_signature(node, source))
    }

    fn get_visibility(&self, node: SyntaxNode<'_>, source: &str) -> Option<Visibility> {
        for child in named_children(node) {
            if child.kind() != "visibility" {
                continue;
            }
            return match get_node_text(child, source).trim() {
                "public" | "external" => Some(Visibility::Public),
                "private" => Some(Visibility::Private),
                "internal" => Some(Visibility::Internal),
                _ => None,
            };
        }
        None
    }

    fn is_const(&self, node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
        Some(node.kind() == "constant_variable_declaration")
    }

    fn visit_node(&self, node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
        let kind = node.kind();

        if matches!(
            kind,
            "contract_declaration" | "library_declaration" | "interface_declaration"
        ) {
            let (ancestors, name, name_node, body) = {
                let source = ctx.source();
                let ancestors = inheritance_ancestors(node, source);
                let Some(name_node) = get_child_by_field(node, "name") else {
                    return false;
                };
                (
                    ancestors,
                    get_node_text(name_node, source).to_string(),
                    name_node,
                    get_child_by_field(node, "body"),
                )
            };
            let node_kind = if kind == "interface_declaration" {
                NodeKind::Interface
            } else {
                NodeKind::Class
            };
            let Some(created) = ctx.create_node(node_kind, &name, node, NodeExtra::default())
            else {
                return true;
            };
            for ancestor in ancestors {
                ctx.add_unresolved_reference(UnresolvedReference {
                    from_node_id: created.id.clone(),
                    reference_name: ancestor,
                    reference_kind: EdgeKind::Extends,
                    line: node.start_position().row as u32 + 1,
                    column: node.start_position().column as u32,
                    file_path: None,
                    language: None,
                    candidates: None,
                    metadata: None,
                });
            }
            ctx.push_scope(created.id);
            if let Some(body) = body {
                for child in named_children(body) {
                    if child != name_node {
                        ctx.visit_node(child);
                    }
                }
            }
            ctx.pop_scope();
            return true;
        }

        if matches!(kind, "struct_declaration" | "enum_declaration") {
            let Some(name_node) = get_child_by_field(node, "name") else {
                return true;
            };
            let name = get_node_text(name_node, ctx.source()).to_string();
            let node_kind = if kind == "struct_declaration" {
                NodeKind::Struct
            } else {
                NodeKind::Enum
            };
            let Some(created) = ctx.create_node(node_kind, &name, node, NodeExtra::default())
            else {
                return true;
            };
            ctx.push_scope(created.id);
            for child in named_children(node) {
                if child != name_node {
                    ctx.visit_node(child);
                }
            }
            ctx.pop_scope();
            return true;
        }

        if kind == "enum_value" {
            let name = get_node_text(node, ctx.source()).to_string();
            ctx.create_node(NodeKind::EnumMember, &name, node, NodeExtra::default());
            return true;
        }

        if matches!(kind, "event_definition" | "error_declaration") {
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
            return true;
        }

        false
    }

    fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
        let signature = get_node_text(node, source).trim().to_string();
        let Some(source_field) = get_child_by_field(node, "source") else {
            return ImportOutcome::Declined;
        };
        let raw_module = first_descendant(source_field, "string_literal")
            .map(|string| get_node_text(string, source))
            .unwrap_or_else(|| get_node_text(source_field, source));
        let module_name = strip_string_quotes(raw_module.trim()).trim();
        if module_name.is_empty() {
            return ImportOutcome::Declined;
        }
        ImportOutcome::Info(ImportInfo::new(module_name, signature))
    }
}

//! R language extraction config.
//!
//! Ported from `src/extraction/languages/r.ts`.

use super::named_children;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{
    ExtractorContext,
    LanguageExtractor,
    NodeExtra,
    SyntaxNode,
};
use crate::types::{EdgeKind, NodeKind, UnresolvedReference};

const ASSIGN_LEFT: &[&str] = &["<-", "<<-", "="];
const ASSIGN_RIGHT: &[&str] = &["->", "->>"];
const IMPORT_FNS: &[&str] = &["library", "require", "requireNamespace", "loadNamespace"];
const CLASS_FNS: &[&str] = &["setClass", "setRefClass", "R6Class", "ggproto"];
const GENERIC_FNS: &[&str] = &["setGeneric", "setMethod"];

fn callee_name(call: SyntaxNode<'_>, source: &str) -> Option<String> {
    let function = get_child_by_field(call, "function")?;
    match function.kind() {
        "identifier" => Some(get_node_text(function, source).to_string()),
        "namespace_operator" => {
            get_child_by_field(function, "rhs").map(|rhs| get_node_text(rhs, source).to_string())
        }
        _ => None,
    }
}

fn first_arg_value(call: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    let arguments = get_child_by_field(call, "arguments")?;
    named_children(arguments)
        .into_iter()
        .find(|argument| argument.kind() == "argument")
        .and_then(|argument| get_child_by_field(argument, "value"))
}

fn literal_or_identifier(node: SyntaxNode<'_>, source: &str) -> Option<String> {
    match node.kind() {
        "identifier" => Some(get_node_text(node, source).to_string()),
        "string" => Some(
            named_children(node)
                .into_iter()
                .find(|child| child.kind() == "string_content")
                .map(|child| get_node_text(child, source))
                .unwrap_or("")
                .to_string(),
        ),
        _ => None,
    }
}

fn is_constant_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_uppercase())
        && chars.all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '.' || ch == '_')
}

fn emit_method_arg(entry: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) {
    let Some(name_node) = get_child_by_field(entry, "name") else {
        return;
    };
    let Some(value) = get_child_by_field(entry, "value") else {
        return;
    };
    if value.kind() != "function_definition" {
        return;
    }

    let (name, signature) = {
        let source = ctx.source();
        let signature = get_child_by_field(value, "parameters")
            .map(|params| get_node_text(params, source).to_string());
        (get_node_text(name_node, source).to_string(), signature)
    };
    let method = ctx.create_node(
        NodeKind::Method,
        &name,
        entry,
        NodeExtra {
            signature,
            ..Default::default()
        },
    );
    let body = get_child_by_field(value, "body");
    if let (Some(method), Some(body)) = (method, body) {
        ctx.push_scope(method.id);
        ctx.visit_node(body);
        ctx.pop_scope();
    }
}

fn extract_class_members(
    class_call: SyntaxNode<'_>,
    class_id: &str,
    ctx: &mut dyn ExtractorContext,
) {
    let Some(arguments) = get_child_by_field(class_call, "arguments") else {
        return;
    };
    let mut positional = 0;

    for argument in named_children(arguments) {
        if argument.kind() != "argument" {
            continue;
        }
        let name_node = get_child_by_field(argument, "name");
        let value = get_child_by_field(argument, "value");

        let Some(name_node) = name_node else {
            positional += 1;
            if let Some(value) =
                value.filter(|value| positional == 2 && value.kind() == "identifier")
            {
                let reference_name = get_node_text(value, ctx.source()).to_string();
                ctx.add_unresolved_reference(UnresolvedReference {
                    from_node_id: class_id.to_string(),
                    reference_name,
                    reference_kind: EdgeKind::Extends,
                    line: value.start_position().row as u32 + 1,
                    column: value.start_position().column as u32,
                    file_path: None,
                    language: None,
                    candidates: None,
                    metadata: None,
                });
            }
            continue;
        };

        let argument_name = get_node_text(name_node, ctx.source()).to_string();
        if matches!(argument_name.as_str(), "inherit" | "contains") {
            if let Some(value) = value {
                let parent = literal_or_identifier(value, ctx.source());
                if let Some(parent) = parent.filter(|name| !name.is_empty()) {
                    ctx.add_unresolved_reference(UnresolvedReference {
                        from_node_id: class_id.to_string(),
                        reference_name: parent,
                        reference_kind: EdgeKind::Extends,
                        line: value.start_position().row as u32 + 1,
                        column: value.start_position().column as u32,
                        file_path: None,
                        language: None,
                        candidates: None,
                        metadata: None,
                    });
                }
            }
            continue;
        }

        let Some(value) = value else {
            continue;
        };
        if value.kind() == "function_definition" {
            emit_method_arg(argument, ctx);
            continue;
        }
        if value.kind() == "call" && callee_name(value, ctx.source()).as_deref() == Some("list") {
            if let Some(list_arguments) = get_child_by_field(value, "arguments") {
                for entry in named_children(list_arguments) {
                    if entry.kind() == "argument" {
                        emit_method_arg(entry, ctx);
                    }
                }
            }
        }
    }
}

pub struct RExtractor;

impl LanguageExtractor for RExtractor {
    fn function_types(&self) -> &[&str] {
        &[]
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
        &[]
    }

    fn enum_types(&self) -> &[&str] {
        &[]
    }

    fn type_alias_types(&self) -> &[&str] {
        &[]
    }

    fn import_types(&self) -> &[&str] {
        &[]
    }

    fn call_types(&self) -> &[&str] {
        &["call"]
    }

    fn variable_types(&self) -> &[&str] {
        &[]
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

    fn visit_node(&self, node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
        if node.kind() == "call" {
            let Some(function_name) = callee_name(node, ctx.source()) else {
                return false;
            };

            if IMPORT_FNS.contains(&function_name.as_str()) || function_name == "source" {
                let Some(module_name) = first_arg_value(node)
                    .and_then(|argument| literal_or_identifier(argument, ctx.source()))
                    .filter(|name| !name.is_empty())
                else {
                    return true;
                };
                let signature: String = get_node_text(node, ctx.source())
                    .trim()
                    .chars()
                    .take(100)
                    .collect();
                let import = ctx.create_node(
                    NodeKind::Import,
                    &module_name,
                    node,
                    NodeExtra {
                        signature: Some(signature),
                        ..Default::default()
                    },
                );
                if import.is_some() {
                    if let Some(parent_id) = ctx.node_stack().last().cloned() {
                        ctx.add_unresolved_reference(UnresolvedReference {
                            from_node_id: parent_id,
                            reference_name: module_name,
                            reference_kind: EdgeKind::Imports,
                            line: node.start_position().row as u32 + 1,
                            column: node.start_position().column as u32,
                            file_path: None,
                            language: None,
                            candidates: None,
                            metadata: None,
                        });
                    }
                }
                return true;
            }

            if CLASS_FNS.contains(&function_name.as_str()) {
                let Some(class_name) = first_arg_value(node)
                    .and_then(|argument| literal_or_identifier(argument, ctx.source()))
                    .filter(|name| !name.is_empty())
                else {
                    return false;
                };
                if let Some(class) =
                    ctx.create_node(NodeKind::Class, &class_name, node, NodeExtra::default())
                {
                    ctx.push_scope(class.id.clone());
                    extract_class_members(node, &class.id, ctx);
                    ctx.pop_scope();
                }
                return true;
            }

            if GENERIC_FNS.contains(&function_name.as_str()) {
                let Some(name) = first_arg_value(node)
                    .and_then(|argument| literal_or_identifier(argument, ctx.source()))
                    .filter(|name| !name.is_empty())
                else {
                    return false;
                };
                let implementation = get_child_by_field(node, "arguments").and_then(|arguments| {
                    named_children(arguments).into_iter().find_map(|argument| {
                        if argument.kind() != "argument" {
                            return None;
                        }
                        get_child_by_field(argument, "value")
                            .filter(|value| value.kind() == "function_definition")
                    })
                });
                let signature = implementation
                    .and_then(|implementation| get_child_by_field(implementation, "parameters"))
                    .map(|params| get_node_text(params, ctx.source()).to_string());
                let function = ctx.create_node(
                    NodeKind::Function,
                    &name,
                    node,
                    NodeExtra {
                        signature,
                        ..Default::default()
                    },
                );
                let body = implementation
                    .and_then(|implementation| get_child_by_field(implementation, "body"));
                if let (Some(function), Some(body)) = (function, body) {
                    ctx.push_scope(function.id);
                    ctx.visit_node(body);
                    ctx.pop_scope();
                }
                return true;
            }

            return false;
        }

        if node.kind() != "binary_operator" {
            return false;
        }

        let Some(operator) = get_child_by_field(node, "operator")
            .map(|operator| get_node_text(operator, ctx.source()).to_string())
        else {
            return false;
        };
        let lhs = get_child_by_field(node, "lhs");
        let rhs = get_child_by_field(node, "rhs");

        if ASSIGN_LEFT.contains(&operator.as_str())
            && lhs.is_some_and(|node| node.kind() == "identifier")
            && rhs.is_some_and(|node| node.kind() == "function_definition")
        {
            let (Some(lhs), Some(rhs)) = (lhs, rhs) else {
                return false;
            };
            let (name, signature) = {
                let source = ctx.source();
                let signature = get_child_by_field(rhs, "parameters")
                    .map(|params| get_node_text(params, source).to_string());
                (get_node_text(lhs, source).to_string(), signature)
            };
            let function = ctx.create_node(
                NodeKind::Function,
                &name,
                node,
                NodeExtra {
                    signature,
                    ..Default::default()
                },
            );
            let body = get_child_by_field(rhs, "body");
            if let (Some(function), Some(body)) = (function, body) {
                ctx.push_scope(function.id);
                ctx.visit_node(body);
                ctx.pop_scope();
            }
            return true;
        }

        let top_level = node
            .parent()
            .is_some_and(|parent| parent.kind() == "program");
        if top_level
            && ASSIGN_LEFT.contains(&operator.as_str())
            && lhs.is_some_and(|node| node.kind() == "identifier")
        {
            let Some(rhs) = rhs else {
                return false;
            };
            let Some(lhs) = lhs else {
                return false;
            };
            let rhs_callee = (rhs.kind() == "call")
                .then(|| callee_name(rhs, ctx.source()))
                .flatten();
            let creates_special_node = rhs_callee
                .as_deref()
                .is_some_and(|name| CLASS_FNS.contains(&name) || GENERIC_FNS.contains(&name));
            if !creates_special_node {
                let name = get_node_text(lhs, ctx.source()).to_string();
                let kind = if is_constant_name(&name) {
                    NodeKind::Constant
                } else {
                    NodeKind::Variable
                };
                ctx.create_node(kind, &name, node, NodeExtra::default());
            }
            ctx.visit_node(rhs);
            return true;
        }

        if top_level
            && ASSIGN_RIGHT.contains(&operator.as_str())
            && rhs.is_some_and(|node| node.kind() == "identifier")
        {
            let Some(lhs) = lhs else {
                return false;
            };
            let Some(rhs) = rhs else {
                return false;
            };
            let name = get_node_text(rhs, ctx.source()).to_string();
            let kind = if is_constant_name(&name) {
                NodeKind::Constant
            } else {
                NodeKind::Variable
            };
            ctx.create_node(kind, &name, node, NodeExtra::default());
            ctx.visit_node(lhs);
            return true;
        }

        false
    }
}

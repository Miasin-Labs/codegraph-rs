//! Nix language extraction config.
//!
//! Ported from `src/extraction/languages/nix.ts`.

use super::named_children;
use crate::extraction::tree_sitter_helpers::get_node_text;
use crate::extraction::tree_sitter_types::{
    ExtractorContext,
    LanguageExtractor,
    NodeExtra,
    SyntaxNode,
};
use crate::types::{EdgeKind, NodeKind, UnresolvedReference};

fn unwrap_variable_expression(node: SyntaxNode<'_>) -> SyntaxNode<'_> {
    if node.kind() == "variable_expression" {
        node.named_child(0).unwrap_or(node)
    } else {
        node
    }
}

fn get_callee_name(node: SyntaxNode<'_>, source: &str) -> Option<String> {
    let mut current = node;
    while current.kind() == "apply_expression" {
        let Some(function) = current
            .child_by_field_name("function")
            .or_else(|| current.named_child(0))
        else {
            break;
        };
        current = function;
    }
    current = unwrap_variable_expression(current);
    matches!(current.kind(), "identifier" | "select_expression")
        .then(|| get_node_text(current, source).trim().to_string())
        .filter(|name| !name.is_empty())
}

fn get_direct_callee_name(node: SyntaxNode<'_>, source: &str) -> Option<String> {
    let function = node
        .child_by_field_name("function")
        .or_else(|| node.named_child(0))?;
    let name = get_node_text(unwrap_variable_expression(function), source)
        .trim()
        .to_string();
    (!name.is_empty()).then_some(name)
}

fn is_static_project_path(value: &str) -> bool {
    (value.starts_with("./") || value.starts_with("../"))
        && !value.chars().any(|character| {
            character.is_whitespace()
                || matches!(
                    character,
                    '{' | '}' | '(' | ')' | '[' | ']' | ';' | '"' | '\'' | '<' | '>' | '$'
                )
        })
}

fn get_static_import_path(argument: SyntaxNode<'_>, source: &str) -> Option<String> {
    let mut current = argument;
    while current.kind() == "parenthesized_expression" {
        let Some(inner) = current.named_child(0) else {
            break;
        };
        current = inner;
    }

    let mut text = get_node_text(current, source).trim();
    if text.len() >= 2 {
        let quoted = (text.starts_with('"') && text.ends_with('"'))
            || (text.starts_with('\'') && text.ends_with('\''));
        if quoted {
            text = &text[1..text.len() - 1];
        }
    }
    is_static_project_path(text).then(|| text.to_string())
}

fn is_returned_attrset_member(node: SyntaxNode<'_>) -> bool {
    let mut current = node;
    let mut seen_returned_attrset = false;

    while let Some(parent) = current.parent() {
        if parent.kind() == "let_expression" {
            let body = parent
                .child_by_field_name("body")
                .or_else(|| parent.child_by_field_name("expression"));
            if body != Some(current) {
                return false;
            }
        }
        if parent.kind() == "binding" && current != node {
            return false;
        }
        if matches!(parent.kind(), "formal_parameters" | "formals") {
            return false;
        }
        if matches!(
            parent.kind(),
            "attrset" | "rec_attrset" | "attrset_expression" | "rec_attrset_expression"
        ) {
            seen_returned_attrset = true;
        }
        current = parent;
    }

    seen_returned_attrset
}

fn get_curried_params_and_body<'tree>(
    node: SyntaxNode<'tree>,
    source: &str,
) -> (Vec<String>, Option<SyntaxNode<'tree>>) {
    let mut parameters = Vec::new();
    let mut current = node;

    while current.kind() == "function_expression" && current.named_child_count() > 0 {
        let Some(body) = current.named_child(current.named_child_count() as u32 - 1) else {
            break;
        };
        let parameter_part = source
            .get(current.start_byte()..body.start_byte())
            .unwrap_or("")
            .trim();
        let parameter = parameter_part
            .strip_suffix(':')
            .unwrap_or(parameter_part)
            .trim();
        if !parameter.is_empty() {
            parameters.push(parameter.to_string());
        }
        if body.kind() == "function_expression" {
            current = body;
        } else {
            return (parameters, Some(body));
        }
    }

    let body = (current.named_child_count() > 0)
        .then(|| current.named_child(current.named_child_count() as u32 - 1))
        .flatten();
    (parameters, body)
}

fn format_function_signature(parameters: &[String]) -> String {
    match parameters {
        [] => "()".to_string(),
        [parameter]
            if parameter.starts_with('(') || parameter.contains('{') || parameter.contains('@') =>
        {
            parameter.clone()
        }
        [parameter] => format!("({parameter})"),
        _ => parameters.join(" : "),
    }
}

fn inherited_attrs(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    named_children(node)
        .into_iter()
        .find(|child| child.kind() == "inherited_attrs")
}

fn is_call_package_name(name: &str) -> bool {
    matches!(name, "callPackage" | "callPackages")
        || name.ends_with(".callPackage")
        || name.ends_with(".callPackages")
}

fn get_first_apply_argument(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    let mut inner = node;
    loop {
        let function = inner
            .child_by_field_name("function")
            .or_else(|| inner.named_child(0));
        let Some(function) = function.filter(|function| function.kind() == "apply_expression")
        else {
            break;
        };
        inner = function;
    }
    inner
        .child_by_field_name("argument")
        .or_else(|| inner.named_child(1))
}

fn emit_file_import(ctx: &mut dyn ExtractorContext, import_path: &str, anchor: SyntaxNode<'_>) {
    let signature: String = get_node_text(anchor, ctx.source())
        .trim()
        .chars()
        .take(100)
        .collect();
    let import = ctx.create_node(
        NodeKind::Import,
        import_path,
        anchor,
        NodeExtra {
            signature: Some(signature),
            ..Default::default()
        },
    );
    if import.is_some() {
        if let Some(parent_id) = ctx.node_stack().last().cloned() {
            ctx.add_unresolved_reference(UnresolvedReference {
                from_node_id: parent_id,
                reference_name: import_path.to_string(),
                reference_kind: EdgeKind::Imports,
                line: anchor.start_position().row as u32 + 1,
                column: anchor.start_position().column as u32,
                file_path: None,
                language: None,
                candidates: None,
                metadata: None,
            });
        }
    }
}

pub struct NixExtractor;

impl LanguageExtractor for NixExtractor {
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
        &[]
    }

    fn variable_types(&self) -> &[&str] {
        &[]
    }

    fn name_field(&self) -> &str {
        ""
    }

    fn body_field(&self) -> &str {
        ""
    }

    fn params_field(&self) -> &str {
        ""
    }

    fn visit_node(&self, node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
        if node.kind() == "binding" {
            let Some(attrpath) = node
                .child_by_field_name("attrpath")
                .or_else(|| node.named_child(0))
            else {
                return false;
            };
            let name = get_node_text(attrpath, ctx.source()).trim().to_string();
            if name.is_empty() {
                return false;
            }
            let Some(value) = node
                .child_by_field_name("expression")
                .or_else(|| node.child_by_field_name("value"))
                .or_else(|| node.named_child(1))
            else {
                return false;
            };

            if value.kind() == "function_expression" {
                let (parameters, body) = get_curried_params_and_body(value, ctx.source());
                let function = ctx.create_node(
                    NodeKind::Function,
                    &name,
                    node,
                    NodeExtra {
                        signature: Some(format_function_signature(&parameters)),
                        is_exported: Some(is_returned_attrset_member(node)),
                        ..Default::default()
                    },
                );
                if let Some(function) = function {
                    ctx.push_scope(function.id);
                    if let Some(body) = body {
                        ctx.visit_node(body);
                    }
                    ctx.pop_scope();
                }
            } else {
                let value_text = get_node_text(value, ctx.source());
                let initial_value: String = value_text.chars().take(100).collect();
                let signature = (!initial_value.is_empty()).then(|| {
                    let ellipsis = if value_text.chars().count() >= 100 {
                        "..."
                    } else {
                        ""
                    };
                    format!("= {initial_value}{ellipsis}")
                });
                ctx.create_node(
                    NodeKind::Variable,
                    &name,
                    node,
                    NodeExtra {
                        signature,
                        is_exported: Some(is_returned_attrset_member(node)),
                        ..Default::default()
                    },
                );

                let final_segment = name.rsplit('.').next();
                if matches!(final_segment, Some("imports" | "modules"))
                    && value.kind() == "list_expression"
                {
                    for child in named_children(value) {
                        if child.kind() != "path_expression" {
                            continue;
                        }
                        let entry_path = get_node_text(child, ctx.source()).trim().to_string();
                        if is_static_project_path(&entry_path) {
                            emit_file_import(ctx, &entry_path, child);
                        }
                    }
                }
                ctx.visit_node(value);
            }
            return true;
        }

        if node.kind() == "function_expression" {
            if node.named_child_count() > 0 {
                let body = node.named_child(node.named_child_count() as u32 - 1);
                if let Some(body) = body {
                    ctx.visit_node(body);
                }
            }
            return true;
        }

        if matches!(node.kind(), "inherit" | "inherit_from") {
            if let Some(attributes) = inherited_attrs(node) {
                for child in named_children(attributes) {
                    let name = get_node_text(child, ctx.source()).trim().to_string();
                    if !name.is_empty() {
                        ctx.create_node(
                            NodeKind::Variable,
                            &name,
                            child,
                            NodeExtra {
                                is_exported: Some(is_returned_attrset_member(child)),
                                ..Default::default()
                            },
                        );
                    }
                }
            }
            for child in named_children(node) {
                if child.kind() != "inherited_attrs" {
                    ctx.visit_node(child);
                }
            }
            return true;
        }

        if node.kind() != "apply_expression" {
            return false;
        }

        let direct_callee = get_direct_callee_name(node, ctx.source());
        let direct_import = matches!(direct_callee.as_deref(), Some("import" | "builtins.import"));
        let parent_function = node.parent().and_then(|parent| {
            (parent.kind() == "apply_expression")
                .then(|| {
                    parent
                        .child_by_field_name("function")
                        .or_else(|| parent.named_child(0))
                })
                .flatten()
        });
        let callee_of_parent = parent_function == Some(node);

        if !(callee_of_parent && !direct_import) {
            if direct_import {
                let argument = node
                    .child_by_field_name("argument")
                    .or_else(|| node.named_child(1));
                if let Some(import_path) =
                    argument.and_then(|argument| get_static_import_path(argument, ctx.source()))
                {
                    emit_file_import(ctx, &import_path, node);
                }
            } else {
                let callee = get_callee_name(node, ctx.source());
                if let Some(callee) = callee
                    .as_deref()
                    .filter(|callee| !matches!(*callee, "import" | "builtins.import"))
                {
                    if let Some(parent_id) = ctx.node_stack().last().cloned() {
                        ctx.add_unresolved_reference(UnresolvedReference {
                            from_node_id: parent_id,
                            reference_name: callee.to_string(),
                            reference_kind: EdgeKind::Calls,
                            line: node.start_position().row as u32 + 1,
                            column: node.start_position().column as u32,
                            file_path: None,
                            language: None,
                            candidates: None,
                            metadata: None,
                        });
                    }
                }

                if callee.as_deref().is_some_and(is_call_package_name) {
                    if let Some(import_path) = get_first_apply_argument(node)
                        .and_then(|argument| get_static_import_path(argument, ctx.source()))
                    {
                        emit_file_import(ctx, &import_path, node);
                    }
                }
            }
        }

        for child in named_children(node) {
            ctx.visit_node(child);
        }
        true
    }
}

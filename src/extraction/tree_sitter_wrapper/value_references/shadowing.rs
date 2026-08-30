use std::collections::HashMap;

use super::super::context::named_children;
use super::declarations::{c_declarator_identifier, first_simple_identifier};
use super::{MAX_VALUE_REFERENCE_NODES, ValueReferenceState};
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::SyntaxNode;

pub(super) fn prune_shadowed_targets(
    root: SyntaxNode<'_>,
    source: &str,
    state: &mut ValueReferenceState,
) {
    let mut declaration_counts = HashMap::new();
    let mut stack = vec![root];
    let mut visited = 0;
    while let Some(node) = stack.pop() {
        if visited >= MAX_VALUE_REFERENCE_NODES {
            break;
        }
        visited += 1;
        count_declaration(node, source, &state.targets, &mut declaration_counts);
        stack.extend(named_children(node));
    }
    for (name, count) in declaration_counts {
        if count > state.target_counts.get(&name).copied().unwrap_or(1) {
            state.targets.remove(&name);
        }
    }
}

fn count_declaration(
    node: SyntaxNode<'_>,
    source: &str,
    targets: &HashMap<String, String>,
    counts: &mut HashMap<String, usize>,
) {
    let mut bump = |name_node: Option<SyntaxNode<'_>>| {
        let Some(name_node) = name_node else { return };
        if !matches!(name_node.kind(), "identifier" | "simple_identifier") {
            return;
        }
        let name = get_node_text(name_node, source);
        if targets.contains_key(name) {
            *counts.entry(name.to_string()).or_default() += 1;
        }
    };

    match node.kind() {
        "variable_declarator" | "const_spec" | "var_spec" => bump(node.named_child(0)),
        "const_item" | "static_item" | "declConst" | "declVar" => {
            bump(get_child_by_field(node, "name"));
        }
        "let_declaration" | "short_var_declaration" | "assignment" => {
            let left = get_child_by_field(node, "left")
                .or_else(|| get_child_by_field(node, "pattern"))
                .or_else(|| node.named_child(0));
            if left.is_some_and(|left| left.kind() == "identifier") {
                bump(left);
            } else if let Some(left) = left {
                for child in named_children(left) {
                    bump(Some(child));
                }
            }
        }
        "init_declarator" => bump(c_declarator_identifier(node)),
        "val_definition" | "var_definition" => {
            let pattern = get_child_by_field(node, "pattern");
            if pattern.is_some_and(|pattern| pattern.kind() == "identifier") {
                bump(pattern);
            }
        }
        "static_final_declaration"
        | "initialized_identifier"
        | "initialized_variable_definition" => {
            bump(
                named_children(node)
                    .into_iter()
                    .find(|child| child.kind() == "identifier"),
            );
        }
        "property_declaration" => {
            let variable = named_children(node)
                .into_iter()
                .find(|child| child.kind() == "variable_declaration");
            let identifier = variable
                .and_then(|variable| {
                    named_children(variable)
                        .into_iter()
                        .find(|child| matches!(child.kind(), "identifier" | "simple_identifier"))
                })
                .or_else(|| {
                    first_simple_identifier(get_child_by_field(node, "name").or_else(|| {
                        named_children(node).into_iter().find(|child| {
                            matches!(child.kind(), "value_binding_pattern" | "pattern")
                        })
                    }))
                });
            bump(identifier);
        }
        _ => {}
    }
}

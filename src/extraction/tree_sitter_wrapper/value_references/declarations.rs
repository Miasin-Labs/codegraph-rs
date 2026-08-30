use super::super::context::named_children;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::SyntaxNode;

pub(in crate::extraction::tree_sitter_wrapper) fn c_declarator_identifier(
    node: SyntaxNode<'_>,
) -> Option<SyntaxNode<'_>> {
    let mut current = Some(node);
    for _ in 0..12 {
        let node = current?;
        match node.kind() {
            "identifier" => return Some(node),
            "function_declarator" => return None,
            "init_declarator"
            | "pointer_declarator"
            | "array_declarator"
            | "parenthesized_declarator" => {
                current = get_child_by_field(node, "declarator");
            }
            _ => return None,
        }
    }
    None
}

pub(in crate::extraction::tree_sitter_wrapper) fn swift_property_info<'tree>(
    node: SyntaxNode<'tree>,
    source: &str,
) -> (Option<SyntaxNode<'tree>>, bool, bool) {
    let pattern = get_child_by_field(node, "name").or_else(|| {
        named_children(node)
            .into_iter()
            .find(|child| matches!(child.kind(), "value_binding_pattern" | "pattern"))
    });
    let binding = named_children(node)
        .into_iter()
        .find(|child| child.kind() == "value_binding_pattern");
    let is_let = binding.is_some_and(|binding| {
        get_node_text(binding, source)
            .trim_start()
            .starts_with("let")
    });
    let is_computed = named_children(node).into_iter().any(|child| {
        matches!(
            child.kind(),
            "computed_property" | "protocol_property_requirements"
        )
    });
    (first_simple_identifier(pattern), is_let, is_computed)
}

pub(super) fn first_simple_identifier(node: Option<SyntaxNode<'_>>) -> Option<SyntaxNode<'_>> {
    let mut queue = node.into_iter().collect::<std::collections::VecDeque<_>>();
    for _ in 0..40 {
        let node = queue.pop_front()?;
        if node.kind() == "simple_identifier" {
            return Some(node);
        }
        queue.extend(named_children(node));
    }
    None
}

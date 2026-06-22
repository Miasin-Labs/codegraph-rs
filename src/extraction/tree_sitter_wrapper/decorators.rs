use super::context::*;
use super::extractor::TreeSitterExtractor;
use crate::extraction::tree_sitter_helpers::*;
use crate::extraction::tree_sitter_types::*;
use crate::types::*;

impl<'a> TreeSitterExtractor<'a> {
    /// Consider one node as a decorator/annotation attached to `decorated_id`
    /// (the TS `consider` closure inside extractDecoratorsFor).
    pub(super) fn consider_decorator(&mut self, n: SyntaxNode<'_>, decorated_id: &str) {
        // `marker_annotation` is Java's grammar for arg-less annotations
        // (`@Override`, `@Deprecated`); without including it, every
        // such Java annotation would be silently skipped.
        if !matches!(n.kind(), "decorator" | "annotation" | "marker_annotation") {
            return;
        }
        // Find the leading identifier: skip the `@` punct, unwrap
        // a call_expression if the decorator is invoked with args.
        let mut target: Option<SyntaxNode<'_>> = None;
        for child in named_children(n) {
            if child.kind() == "call_expression" {
                let f = get_child_by_field(child, "function").or_else(|| child.named_child(0));
                if let Some(f) = f {
                    target = Some(f);
                }
                if target.is_some() {
                    break;
                }
            }
            if matches!(
                child.kind(),
                "identifier" | "member_expression" | "scoped_identifier" | "navigation_expression"
            ) {
                target = Some(child);
                break;
            }
        }
        let Some(target) = target else { return };
        let name = strip_qualifier(get_node_text(target, self.source));
        if name.is_empty() {
            return;
        }
        self.unresolved_references.push(UnresolvedReference {
            from_node_id: decorated_id.to_string(),
            reference_name: name,
            reference_kind: EdgeKind::Decorates,
            line: n.start_position().row as u32 + 1,
            column: n.start_position().column as u32,
            file_path: None,
            language: None,
            candidates: None,
        });
    }

    /// Scan `decl_node` and its preceding siblings (within the parent's
    /// named children) for decorator nodes, emitting a `decorates`
    /// reference from `decorated_id` to each decorator's function name.
    ///
    /// Why preceding siblings: in TypeScript, `@Foo class Bar {}` parses
    /// as an `export_statement` (or top-level wrapper) with the
    /// `decorator` as a child *before* the `class_declaration` — so the
    /// decorator isn't a child of the class itself. For methods/
    /// properties, the decorator IS a direct child of the declaration,
    /// so we also scan decl_node's named children.
    ///
    /// Idempotent across grammars: if neither location yields decorators
    /// (most non-decorator-using languages), the function is a no-op.
    pub(super) fn extract_decorators_for(&mut self, decl_node: SyntaxNode<'_>, decorated_id: &str) {
        // 1. Decorators that are direct children of the declaration
        //    (method/property style, also some grammars for class).
        for child in named_children(decl_node) {
            self.consider_decorator(child, decorated_id);
        }

        // 2. Decorators that are PRECEDING siblings of the declaration
        //    inside the parent's children (TypeScript class style).
        //    Walk BACKWARDS from the declaration and stop at the first
        //    non-decorator sibling — without that stop, decorators
        //    belonging to an EARLIER unrelated declaration leak in
        //    (e.g. `@A class Foo {} @B class Bar {}` would otherwise
        //    attribute @A to Bar).
        //
        //    Note on identity: matching is by start byte (the TS web
        //    bindings return fresh wrapper objects from navigation, so
        //    the original matched on startIndex; kept for parity).
        if let Some(parent) = decl_node.parent() {
            let decl_start = decl_node.start_byte();
            let siblings = named_children(parent);
            let decl_idx = siblings.iter().position(|s| s.start_byte() == decl_start);
            if let Some(decl_idx) = decl_idx {
                if decl_idx > 0 {
                    for j in (0..decl_idx).rev() {
                        let sibling = siblings[j];
                        if !matches!(
                            sibling.kind(),
                            "decorator" | "annotation" | "marker_annotation"
                        ) {
                            break; // non-decorator separator → stop consuming
                        }
                        self.consider_decorator(sibling, decorated_id);
                    }
                }
            }
        }
    }
}

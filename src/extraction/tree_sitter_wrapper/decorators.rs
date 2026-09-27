use super::context::{named_children, strip_qualifier};
use super::extractor::TreeSitterExtractor;
use super::siblings::{SiblingKind, first_starting_at, is_decorator_kind, run_start};
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::SyntaxNode;
use crate::types::{EdgeKind, Language, UnresolvedReference};

impl<'a> TreeSitterExtractor<'a> {
    /// Consider one node as a decorator/annotation attached to `decorated_id`
    /// (the TS `consider` closure inside extractDecoratorsFor).
    pub(super) fn consider_decorator(&mut self, n: SyntaxNode<'_>, decorated_id: &str) {
        // `marker_annotation` is Java's grammar for arg-less annotations
        // (`@Override`, `@Deprecated`); without including it, every
        // such Java annotation would be silently skipped.
        if !is_decorator_kind(n.kind()) {
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
        if self.language == Language::Vyper
            && matches!(
                name.as_str(),
                "deploy"
                    | "external"
                    | "internal"
                    | "nonpayable"
                    | "nonreentrant"
                    | "payable"
                    | "pure"
                    | "view"
            )
        {
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
            metadata: None,
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
        //
        //    The parent's children come from the per-file sibling index
        //    (`siblings.rs`): walking them per declaration was quadratic.
        //    Only a declaration a decorator precedes walks them again.
        let Some(parent) = decl_node.parent() else {
            return;
        };
        let siblings = self.siblings.of(parent);
        let Some(decl_idx) = first_starting_at(siblings, decl_node.start_byte()) else {
            return;
        };
        let run_start = run_start(siblings, decl_idx, SiblingKind::Decorator);
        if run_start == decl_idx {
            return;
        }
        let siblings = named_children(parent);
        for sibling in siblings[run_start..decl_idx].iter().rev() {
            self.consider_decorator(*sibling, decorated_id);
        }
    }
}

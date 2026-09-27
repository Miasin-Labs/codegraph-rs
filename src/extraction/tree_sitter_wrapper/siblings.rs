//! The named children of a declaration's parent, read once per parent.
//!
//! Every sibling lookup in tree-sitter (`prev_named_sibling`, a fresh
//! `named_children` walk) iterates the parent's children from the first, so
//! doing one per declaration is quadratic in a flat parent: a bundled
//! `cli.js` puts tens of thousands of declarations under `program` and pinned
//! every sync worker for hours. The extractor keeps one [`SiblingIndex`] per
//! file (node ids are stable for the file's one tree) and answers the
//! preceding-decorator and preceding-comment scans from it.

use std::collections::HashMap;

use super::context::named_children;
use super::extractor::TreeSitterExtractor;
use crate::extraction::tree_sitter_helpers::{docstring_from_comments, get_preceding_docstring};
use crate::extraction::tree_sitter_types::SyntaxNode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SiblingKind {
    Decorator,
    Comment,
    Other,
}

impl SiblingKind {
    fn of(kind: &str) -> Self {
        if is_decorator_kind(kind) {
            Self::Decorator
        } else if matches!(
            kind,
            "comment" | "line_comment" | "block_comment" | "documentation_comment"
        ) {
            Self::Comment
        } else {
            Self::Other
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Sibling {
    pub(super) id: usize,
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) kind: SiblingKind,
}

#[derive(Debug, Default)]
pub(super) struct SiblingIndex(HashMap<usize, Vec<Sibling>>);

impl SiblingIndex {
    /// The named children of `parent`, read from the tree the first time.
    pub(super) fn of<'s>(&'s mut self, parent: SyntaxNode<'_>) -> &'s [Sibling] {
        self.0.entry(parent.id()).or_insert_with(|| {
            named_children(parent)
                .into_iter()
                .map(|s| Sibling {
                    id: s.id(),
                    start: s.start_byte(),
                    end: s.end_byte(),
                    kind: SiblingKind::of(s.kind()),
                })
                .collect()
        })
    }
}

pub(super) fn is_decorator_kind(kind: &str) -> bool {
    matches!(kind, "decorator" | "annotation" | "marker_annotation")
}

/// Index of the first sibling starting at `start` (start bytes never
/// decrease along the children).
pub(super) fn first_starting_at(siblings: &[Sibling], start: usize) -> Option<usize> {
    let idx = siblings.partition_point(|s| s.start < start);
    siblings.get(idx).filter(|s| s.start == start).map(|_| idx)
}

/// Index of the sibling that is `node` itself.
pub(super) fn position_of(siblings: &[Sibling], node: SyntaxNode<'_>) -> Option<usize> {
    let from = siblings.partition_point(|s| s.start < node.start_byte());
    siblings[from..]
        .iter()
        .take_while(|s| s.start == node.start_byte())
        .position(|s| s.id == node.id())
        .map(|offset| from + offset)
}

/// Start of the run of `kind` siblings that ends right before `idx`.
pub(super) fn run_start(siblings: &[Sibling], idx: usize, kind: SiblingKind) -> usize {
    siblings[..idx]
        .iter()
        .rposition(|s| s.kind != kind)
        .map_or(0, |other| other + 1)
}

impl TreeSitterExtractor<'_> {
    /// [`get_preceding_docstring`] answered from the sibling index: the
    /// comments right before `node` among its parent's named children.
    pub(super) fn preceding_docstring(&mut self, node: SyntaxNode<'_>) -> Option<String> {
        let parent = node.parent()?;
        let siblings = self.siblings.of(parent);
        let Some(idx) = position_of(siblings, node) else {
            return get_preceding_docstring(node, self.source);
        };
        let from = run_start(siblings, idx, SiblingKind::Comment);
        let comments: Vec<&str> = siblings[from..idx]
            .iter()
            .map(|s| self.source.get(s.start..s.end).unwrap_or(""))
            .collect();
        docstring_from_comments(&comments)
    }
}

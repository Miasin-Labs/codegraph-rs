//! `return`, `break`, `continue`, Go `fallthrough`, jump labels, and the
//! scan for jumps buried inside expressions.

use tree_sitter::Node as TsNode;

use super::{Builder, Cursor, EXIT_ID, lines, unwrap_statement};
use crate::cfg::{CfgBlockKind, CfgEdgeKind, label};
use crate::cfg_rules::{Construct, LabelStyle};

/// Header fields a construct evaluates before branching; scanned for
/// embedded exits (`match parse(s)? { … }`).
const HEADER_FIELDS: &[&str] = &["condition", "value", "subject", "right", "initializer"];
/// Upper bound on the nodes one embedded-exit scan visits.
const SCAN_BUDGET: usize = 4096;

/// What a `break` or `continue` can leave.
#[derive(Debug, Clone, Copy)]
pub(super) enum Target {
    Loop {
        header: u32,
    },
    Switch,
    /// A labeled non-loop statement (`'a: { … }`, JS `a: { … }`); only a
    /// labeled `break` leaves it.
    Block,
}

/// An enclosing jump target and the `break`s that leave it.
pub(super) struct JumpTarget {
    target: Target,
    label: Option<String>,
    breaks: Vec<u32>,
}

impl JumpTarget {
    pub(super) fn new(target: Target, label: Option<String>) -> Self {
        Self {
            target,
            label,
            breaks: Vec::new(),
        }
    }
}

impl Builder<'_, '_> {
    fn jump_block(&mut self, at: Cursor, label: &str, node: TsNode<'_>) -> u32 {
        self.enter(at, label, CfgBlockKind::Normal, node)
    }

    pub(super) fn walk_return(&mut self, node: TsNode<'_>, at: Cursor) -> Option<Cursor> {
        let id = self.jump_block(at, label::RETURN, node);
        self.may_throw(id);
        self.add_edge(id, EXIT_ID, CfgEdgeKind::Return);
        None
    }

    pub(super) fn walk_break(&mut self, node: TsNode<'_>, at: Cursor) -> Option<Cursor> {
        let id = self.jump_block(at, label::BREAK, node);
        self.route_break(id, node);
        None
    }

    pub(super) fn walk_continue(&mut self, node: TsNode<'_>, at: Cursor) -> Option<Cursor> {
        let id = self.jump_block(at, label::CONTINUE, node);
        self.route_continue(id, node);
        None
    }

    pub(super) fn walk_fallthrough(&mut self, node: TsNode<'_>, at: Cursor) -> Option<Cursor> {
        let in_switch = self
            .targets
            .iter()
            .any(|target| matches!(target.target, Target::Switch));
        if !in_switch {
            return Some(self.walk_plain(node, at));
        }
        self.fallthrough.push(at);
        None
    }

    pub(super) fn walk_labeled(&mut self, node: TsNode<'_>, at: Cursor) -> Option<Cursor> {
        let label_kind = self.rules.labels.label_kind();
        let mut name = None;
        let mut inner = None;
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.is_extra() {
                continue;
            }
            if name.is_none() && Some(child.kind()) == label_kind {
                name = Some(self.text(child).to_owned());
            } else {
                inner = Some(child);
            }
        }
        let Some(inner) = inner else {
            return Some(at);
        };
        let wraps_target = matches!(
            self.rules.classify(unwrap_statement(inner).kind()),
            Construct::Loop | Construct::InfiniteLoop | Construct::Switch
        );
        if wraps_target {
            self.pending_label = name;
            return self.walk_stmt(inner, at);
        }
        // A labeled non-loop statement: only `break label` leaves it.
        self.targets.push(JumpTarget::new(Target::Block, name));
        let end = self.walk_stmt(inner, at);
        self.close_target(end, label::AFTER_LABEL, lines(node).1)
    }

    fn route_break(&mut self, from: u32, node: TsNode<'_>) {
        let name = self.jump_label(node);
        let target = self.targets.iter_mut().rev().find(|target| match &name {
            Some(name) => target.label.as_deref() == Some(name.as_str()),
            None => !matches!(target.target, Target::Block),
        });
        if let Some(target) = target {
            target.breaks.push(from);
        }
    }

    fn route_continue(&mut self, from: u32, node: TsNode<'_>) {
        let name = self.jump_label(node);
        let header = self
            .targets
            .iter()
            .rev()
            .find_map(|target| match target.target {
                Target::Loop { header }
                    if name.is_none() || target.label.as_deref() == name.as_deref() =>
                {
                    Some(header)
                }
                _ => None,
            });
        if let Some(header) = header {
            self.add_edge(from, header, CfgEdgeKind::Continue);
        }
    }

    /// The label a `break`/`continue` names, if any.
    fn jump_label(&self, node: TsNode<'_>) -> Option<String> {
        let kind = self.rules.labels.label_kind()?;
        let mut cursor = node.walk();
        let label = node
            .named_children(&mut cursor)
            .find(|child| child.kind() == kind)?;
        Some(self.text(label).to_owned())
    }

    /// A loop/switch's label: from its wrapper statement, or leading it.
    pub(super) fn take_label(&mut self, node: TsNode<'_>) -> Option<String> {
        self.pending_label
            .take()
            .or_else(|| self.leading_label(node))
    }

    /// The label leading a loop or block (Rust `'outer:`).
    pub(super) fn leading_label(&self, node: TsNode<'_>) -> Option<String> {
        let LabelStyle::Leading { label } = self.rules.labels else {
            return None;
        };
        let first = node.named_child(0)?;
        (first.kind() == label).then(|| self.text(first).to_owned())
    }

    /// The `break`s that left the innermost target, as pending edges.
    pub(super) fn pop_breaks(&mut self) -> Vec<Cursor> {
        let breaks = self
            .targets
            .pop()
            .map(|target| target.breaks)
            .unwrap_or_default();
        breaks
            .into_iter()
            .map(|from| Cursor::edge(from, CfgEdgeKind::Break))
            .collect()
    }

    /// Pop the innermost target and join its `break`s with `end`.
    pub(super) fn close_target(
        &mut self,
        end: Option<Cursor>,
        label: &str,
        line: u32,
    ) -> Option<Cursor> {
        let mut ends = self.pop_breaks();
        ends.extend(end);
        self.join(ends, label, line)
    }

    // ─── Embedded exits ──────────────────────────────────────────────────

    /// Scan a construct's header (condition, scrutinee) for embedded exits.
    pub(super) fn scan_header(&mut self, node: TsNode<'_>, block: u32) {
        for name in HEADER_FIELDS {
            if let Some(part) = node.child_by_field_name(name) {
                self.scan_exits(part, block);
            }
        }
    }

    /// Add edges from `block` for jumps buried in the expressions of `node`:
    /// a `return` in a match arm, Rust `?`, a `throw` expression, a
    /// `continue` in a `let … else`. Iterative and bounded by
    /// [`SCAN_BUDGET`] nodes; never enters nested function scopes, and
    /// `break`/`continue` inside a nested loop stay there. Returns whether
    /// any exit was found.
    pub(super) fn scan_exits(&mut self, node: TsNode<'_>, block: u32) -> bool {
        let mut found = false;
        let mut stack = vec![(node, false)];
        let mut budget = SCAN_BUDGET;
        while let Some((current, in_nested_loop)) = stack.pop() {
            if budget == 0 {
                break;
            }
            budget -= 1;
            let kind = current.kind();
            if self.rules.is_nested_scope(kind) {
                continue;
            }
            let construct = self.rules.classify(kind);
            match construct {
                Construct::Return => {
                    self.add_edge(block, EXIT_ID, CfgEdgeKind::Return);
                    found = true;
                }
                _ if Some(kind) == self.rules.try_operator => {
                    self.add_edge(block, EXIT_ID, CfgEdgeKind::Return);
                    found = true;
                }
                Construct::Throw => {
                    self.raise(block, true);
                    found = true;
                }
                Construct::Break if !in_nested_loop => {
                    self.route_break(block, current);
                    found = true;
                }
                Construct::Continue if !in_nested_loop => {
                    self.route_continue(block, current);
                    found = true;
                }
                _ => {}
            }
            let nested = in_nested_loop
                || matches!(construct, Construct::Loop | Construct::InfiniteLoop)
                || (construct == Construct::Switch && self.rules.break_exits_switch);
            let mut cursor = current.walk();
            stack.extend(
                current
                    .named_children(&mut cursor)
                    .map(|child| (child, nested)),
            );
        }
        found
    }
}

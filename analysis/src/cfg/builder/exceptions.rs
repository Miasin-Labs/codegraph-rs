//! `try`/`catch`/`finally`, `throw`, and the exception edges of code inside
//! a protected region.

use tree_sitter::Node as TsNode;

use super::{Builder, Cursor, EXIT_ID, lines};
use crate::cfg::{CfgBlockKind, CfgEdgeKind, label};
use crate::cfg_rules::CfgRules;

/// An exception-protected region: a `try` body, or a `catch` body (and
/// Python `else:`) guarded by a `finally`.
pub(super) struct Region {
    /// Handler entry blocks that catch exceptions raised here.
    catches: Vec<u32>,
    /// With a `finally`: blocks whose exceptions run it before rethrowing.
    to_finally: Option<Vec<u32>>,
}

impl Region {
    fn handles(&self) -> bool {
        !self.catches.is_empty() || self.to_finally.is_some()
    }
}

/// The parts of a try statement, found by kind.
struct TryParts<'t> {
    body: Option<TsNode<'t>>,
    catches: Vec<TsNode<'t>>,
    finally: Option<TsNode<'t>>,
    /// Python `try: … else:`, run when the body raised nothing.
    orelse: Option<TsNode<'t>>,
}

impl<'t> TryParts<'t> {
    fn of(node: TsNode<'t>, rules: &CfgRules) -> Self {
        let mut parts = Self {
            body: node.child_by_field_name("body"),
            catches: Vec::new(),
            finally: None,
            orelse: None,
        };
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            let kind = Some(child.kind());
            if kind == rules.catch_node {
                parts.catches.push(child);
            } else if kind == rules.finally_node {
                parts.finally = Some(child);
            } else if kind == rules.else_node {
                parts.orelse = Some(child);
            }
        }
        parts
    }
}

impl Builder<'_, '_> {
    pub(super) fn walk_try(&mut self, node: TsNode<'_>, at: Cursor) -> Option<Cursor> {
        let line = lines(node).1;
        let parts = TryParts::of(node, self.rules);
        let header = self.enter(at, label::TRY, CfgBlockKind::Normal, node);
        let mut catches = Vec::with_capacity(parts.catches.len());
        for &clause in &parts.catches {
            let (start, end) = lines(clause);
            let id = self.push_block(label::CATCH, CfgBlockKind::Exception, start, end);
            self.add_edge(header, id, CfgEdgeKind::Exception);
            catches.push((clause, id));
        }
        let has_finally = parts.finally.is_some();

        // The protected body: its exceptions reach the catches (or finally).
        self.regions.push(Region {
            catches: catches.iter().map(|&(_, id)| id).collect(),
            to_finally: has_finally.then(Vec::new),
        });
        let body_end = self.walk_try_body(node, parts.body, Cursor::after(header));
        let mut raised = self.pop_region();

        // Handlers and Python's `else:` run outside the body's protection,
        // but still under the `finally`.
        if has_finally {
            self.regions.push(Region {
                catches: Vec::new(),
                to_finally: Some(Vec::new()),
            });
        }
        let mut ends = Vec::new();
        if let Some(end) = body_end {
            ends.extend(match parts.orelse {
                Some(orelse) => self.walk_stmt(orelse, end),
                None => Some(end),
            });
        }
        for (clause, id) in catches {
            ends.extend(self.walk_clause(clause, Cursor::after(id)));
        }
        if has_finally {
            raised.extend(self.pop_region());
        }

        match parts.finally {
            Some(finally) => self.walk_finally(finally, ends, &raised, line),
            None => self.join(ends, label::AFTER_TRY, line),
        }
    }

    /// Walk a try body: its `body` field, or (Erlang `try … of … catch`)
    /// every child that is not a handler clause.
    fn walk_try_body(
        &mut self,
        node: TsNode<'_>,
        body: Option<TsNode<'_>>,
        at: Cursor,
    ) -> Option<Cursor> {
        if let Some(body) = body {
            return self.walk_stmt(body, at);
        }
        let rules = self.rules;
        self.walk_children(node, at, |child| {
            let kind = Some(child.kind());
            kind != rules.catch_node && kind != rules.finally_node && kind != rules.else_node
        })
    }

    /// Walk a handler clause: its `body` field, or its children.
    fn walk_clause(&mut self, clause: TsNode<'_>, at: Cursor) -> Option<Cursor> {
        match clause.child_by_field_name("body") {
            Some(body) => self.walk_stmt(body, at),
            None => self.walk_seq(clause, at),
        }
    }

    /// Run the `finally` on every way out of the try that it intercepts: the
    /// normal `ends`, and the blocks that `raised` (rethrown afterwards).
    fn walk_finally(
        &mut self,
        finally: TsNode<'_>,
        ends: Vec<Cursor>,
        raised: &[u32],
        line: u32,
    ) -> Option<Cursor> {
        if ends.is_empty() && raised.is_empty() {
            return None;
        }
        let completes = !ends.is_empty();
        let (start, end) = lines(finally);
        let id = self.push_block(label::FINALLY, CfgBlockKind::Normal, start, end);
        for from in ends {
            self.add_edge(from.from, id, from.kind);
        }
        for &from in raised {
            self.add_edge(from, id, CfgEdgeKind::Exception);
        }
        let finally_end = self.walk_clause(finally, Cursor::after(id))?;
        if !raised.is_empty() {
            self.raise(finally_end.from, true);
        }
        if completes {
            self.join(vec![finally_end], label::AFTER_TRY, line)
        } else {
            None
        }
    }

    fn pop_region(&mut self) -> Vec<u32> {
        self.regions
            .pop()
            .and_then(|region| region.to_finally)
            .unwrap_or_default()
    }

    pub(super) fn walk_throw(&mut self, node: TsNode<'_>, at: Cursor) -> Option<Cursor> {
        let id = self.enter(at, label::THROW, CfgBlockKind::Normal, node);
        self.raise(id, true);
        None
    }

    /// Route an exception raised in `from` to the innermost handler; when
    /// nothing catches it, an explicit throw (`to_exit`) leaves the function.
    pub(super) fn raise(&mut self, from: u32, to_exit: bool) {
        let Some(index) = self.regions.iter().rposition(Region::handles) else {
            if to_exit {
                self.add_edge(from, EXIT_ID, CfgEdgeKind::Exception);
            }
            return;
        };
        let region = &mut self.regions[index];
        if region.catches.is_empty() {
            if let Some(pending) = region.to_finally.as_mut() {
                pending.push(from);
            }
            return;
        }
        for catch in region.catches.clone() {
            self.add_edge(from, catch, CfgEdgeKind::Exception);
        }
    }

    /// A block evaluating code inside a protected region may throw.
    pub(super) fn may_throw(&mut self, block: u32) {
        self.raise(block, false);
    }
}

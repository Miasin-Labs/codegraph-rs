//! The CFG builder: one language-agnostic walk driven by [`CfgRules`].
//!
//! Control position is a [`Cursor`] — the pending edge into the next block —
//! so a construct that cannot complete normally (every path returned,
//! threw, or jumped) yields `None` and its join block is never created.
//! That keeps two invariants by construction: every block is reachable from
//! ENTRY, and every block other than EXIT or an unresolved jump has a
//! successor.
//!
//! This module walks statements, conditionals, loops, and switches;
//! [`exceptions`] handles `try`/`throw` and [`jumps`] handles
//! `return`/`break`/`continue`, labels, and jumps buried in expressions.

mod exceptions;
mod jumps;

use std::collections::HashSet;

use tree_sitter::Node as TsNode;

use self::exceptions::Region;
use self::jumps::{JumpTarget, Target};
use super::{CfgBlock, CfgBlockKind, CfgEdge, CfgEdgeKind, FunctionCfg, label};
use crate::cfg_rules::{CfgRules, Construct, LabelStyle, SwitchDefault};

/// Fields holding a conditional's taken branch (Move spells it `et`).
const CONSEQUENCE_FIELDS: &[&str] = &["consequence", "body", "et"];
/// Fields holding a conditional's alternatives (Move `ef`, Solidity `else`).
const ALTERNATIVE_FIELDS: &[&str] = &["alternative", "ef", "else"];

const ENTRY_ID: u32 = 0;
const EXIT_ID: u32 = 1;

/// Build the CFG of `function`, whose body is `body`.
pub(super) fn build_function(
    function: TsNode<'_>,
    body: TsNode<'_>,
    source: &[u8],
    rules: &CfgRules,
) -> FunctionCfg {
    let mut builder = Builder::new(rules, source);
    let (start, end) = lines(function);
    builder.push_block(label::ENTRY, CfgBlockKind::Entry, start, start);
    builder.push_block(label::EXIT, CfgBlockKind::Exit, end, end);
    if let Some(last) = builder.walk_body(body, Cursor::after(ENTRY_ID)) {
        builder.add_edge(last.from, EXIT_ID, last.kind);
    }
    builder.finish()
}

/// The pending edge into the next block: where control stands between blocks.
#[derive(Debug, Clone, Copy)]
struct Cursor {
    from: u32,
    kind: CfgEdgeKind,
    /// `from` is a straight-line block the next plain statement may extend.
    open: bool,
}

impl Cursor {
    fn after(from: u32) -> Self {
        Self::edge(from, CfgEdgeKind::Normal)
    }

    fn edge(from: u32, kind: CfgEdgeKind) -> Self {
        Self {
            from,
            kind,
            open: false,
        }
    }
}

struct Builder<'s, 'r> {
    rules: &'r CfgRules,
    source: &'s [u8],
    blocks: Vec<CfgBlock>,
    edges: Vec<CfgEdge>,
    seen_edges: HashSet<(u32, u32, CfgEdgeKind)>,
    /// Enclosing loops, switches, and labeled blocks, innermost last.
    targets: Vec<JumpTarget>,
    /// Enclosing exception-protected regions, innermost last.
    regions: Vec<Region>,
    /// A wrapper statement's label, consumed by the loop/switch it wraps.
    pending_label: Option<String>,
    /// Go `fallthrough`s awaiting the next case of the innermost switch.
    fallthrough: Vec<Cursor>,
}

impl<'s, 'r> Builder<'s, 'r> {
    fn new(rules: &'r CfgRules, source: &'s [u8]) -> Self {
        Self {
            rules,
            source,
            blocks: Vec::new(),
            edges: Vec::new(),
            seen_edges: HashSet::new(),
            targets: Vec::new(),
            regions: Vec::new(),
            pending_label: None,
            fallthrough: Vec::new(),
        }
    }

    fn finish(self) -> FunctionCfg {
        FunctionCfg {
            blocks: self.blocks,
            edges: self.edges,
        }
    }

    // ─── Graph primitives ────────────────────────────────────────────────

    fn push_block(
        &mut self,
        label: &str,
        kind: CfgBlockKind,
        start_line: u32,
        end_line: u32,
    ) -> u32 {
        let id = self.blocks.len() as u32;
        self.blocks.push(CfgBlock {
            id,
            label: label.to_owned(),
            start_line,
            end_line,
            kind,
        });
        id
    }

    fn add_edge(&mut self, from: u32, to: u32, kind: CfgEdgeKind) {
        if self.seen_edges.insert((from, to, kind)) {
            self.edges.push(CfgEdge { from, to, kind });
        }
    }

    /// Create a block for `node` and enter it over the pending edge `at`.
    fn enter(&mut self, at: Cursor, label: &str, kind: CfgBlockKind, node: TsNode<'_>) -> u32 {
        let (start, end) = lines(node);
        let id = self.push_block(label, kind, start, end);
        self.add_edge(at.from, id, at.kind);
        id
    }

    /// Merge `ends` into a fresh join block; `None` when nothing reaches it.
    fn join(&mut self, ends: Vec<Cursor>, label: &str, line: u32) -> Option<Cursor> {
        if ends.is_empty() {
            return None;
        }
        let id = self.push_block(label, CfgBlockKind::Normal, line, line);
        for end in ends {
            self.add_edge(end.from, id, end.kind);
        }
        Some(Cursor::after(id))
    }

    fn text(&self, node: TsNode<'_>) -> &'s str {
        node.utf8_text(self.source).unwrap_or_default()
    }

    // ─── Statements ──────────────────────────────────────────────────────

    /// Walk a function body. A body that is neither a container nor a
    /// construct (Nix `let … in`, an arrow function's expression) is walked
    /// as the sequence of its children.
    fn walk_body(&mut self, body: TsNode<'_>, at: Cursor) -> Option<Cursor> {
        match self.rules.classify(body.kind()) {
            Construct::Plain => self.walk_seq(body, at),
            _ => self.walk_stmt(body, at),
        }
    }

    /// Walk one statement from `at`; `None` when it cannot complete normally.
    /// The recursion head of the builder.
    fn walk_stmt(&mut self, node: TsNode<'_>, at: Cursor) -> Option<Cursor> {
        crate::ensure_sufficient_stack(|| self.dispatch(unwrap_statement(node), at))
    }

    fn dispatch(&mut self, node: TsNode<'_>, at: Cursor) -> Option<Cursor> {
        match self.rules.classify(node.kind()) {
            Construct::Block => self.walk_block(node, at),
            Construct::If => self.walk_if(node, at),
            Construct::Loop => self.walk_loop(node, at, self.has_loop_header(node)),
            Construct::InfiniteLoop => self.walk_loop(node, at, false),
            Construct::Switch => self.walk_switch(node, at),
            Construct::Try => self.walk_try(node, at),
            Construct::Labeled => self.walk_labeled(node, at),
            Construct::Return => self.walk_return(node, at),
            Construct::Throw => self.walk_throw(node, at),
            Construct::Break => self.walk_break(node, at),
            Construct::Continue => self.walk_continue(node, at),
            Construct::Fallthrough => self.walk_fallthrough(node, at),
            Construct::Plain => Some(self.walk_plain(node, at)),
        }
    }

    /// Walk the named children of `node` in order, stopping at the first
    /// that cannot complete normally (the rest is dead code).
    fn walk_seq(&mut self, node: TsNode<'_>, at: Cursor) -> Option<Cursor> {
        self.walk_children(node, at, |_| true)
    }

    fn walk_children(
        &mut self,
        node: TsNode<'_>,
        mut at: Cursor,
        keep: impl Fn(&TsNode<'_>) -> bool,
    ) -> Option<Cursor> {
        let leading_label = match self.rules.labels {
            LabelStyle::Leading { label } => Some(label),
            _ => None,
        };
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.is_extra() || Some(child.kind()) == leading_label || !keep(&child) {
                continue;
            }
            at = self.walk_stmt(child, at)?;
        }
        Some(at)
    }

    fn walk_block(&mut self, node: TsNode<'_>, at: Cursor) -> Option<Cursor> {
        let Some(name) = self.leading_label(node) else {
            return self.walk_seq(node, at);
        };
        // A labeled block (`'a: { … break 'a; … }`).
        self.targets
            .push(JumpTarget::new(Target::Block, Some(name)));
        let end = self.walk_seq(node, at);
        self.close_target(end, label::AFTER_LABEL, lines(node).1)
    }

    /// A straight-line statement: extend the open block or start one, then
    /// add edges for any jump buried in its expressions.
    fn walk_plain(&mut self, node: TsNode<'_>, at: Cursor) -> Cursor {
        let block = if at.open {
            if let Some(block) = self.blocks.get_mut(at.from as usize) {
                block.end_line = lines(node).1;
            }
            at.from
        } else {
            let id = self.enter(at, label::STMT, CfgBlockKind::Normal, node);
            self.may_throw(id);
            id
        };
        let exits = self.scan_exits(node, block);
        Cursor {
            from: block,
            kind: CfgEdgeKind::Normal,
            // A block with an early exit ends there, like a MIR terminator.
            open: !exits,
        }
    }

    // ─── Conditionals ────────────────────────────────────────────────────

    fn walk_if(&mut self, node: TsNode<'_>, at: Cursor) -> Option<Cursor> {
        let line = lines(node).1;
        let branch = self.branch_block(at, label::IF, node);
        let mut ends = Vec::new();
        ends.extend(self.walk_taken(field(node, CONSEQUENCE_FIELDS), branch));
        let mut otherwise = Cursor::edge(branch, CfgEdgeKind::BranchFalse);
        for alt in self.alternatives(node) {
            if self.rules.elif_nodes.contains(&alt.kind()) {
                let elif = self.branch_block(otherwise, label::ELIF, alt);
                ends.extend(self.walk_taken(field(alt, CONSEQUENCE_FIELDS), elif));
                otherwise = Cursor::edge(elif, CfgEdgeKind::BranchFalse);
            } else {
                let else_id = self.enter(otherwise, label::ELSE, CfgBlockKind::Normal, alt);
                ends.extend(self.walk_stmt(alt, Cursor::after(else_id)));
                return self.join(ends, label::AFTER_IF, line);
            }
        }
        ends.push(otherwise);
        self.join(ends, label::AFTER_IF, line)
    }

    /// Enter a branch block whose header (condition, scrutinee) may throw
    /// or exit early.
    fn branch_block(&mut self, at: Cursor, label: &str, node: TsNode<'_>) -> u32 {
        let id = self.enter(at, label, CfgBlockKind::Branch, node);
        self.may_throw(id);
        self.scan_header(node, id);
        id
    }

    /// Walk the taken side of a conditional (`None`: an empty consequence).
    fn walk_taken(&mut self, body: Option<TsNode<'_>>, branch: u32) -> Option<Cursor> {
        let at = Cursor::edge(branch, CfgEdgeKind::BranchTrue);
        match body {
            Some(body) => self.walk_stmt(body, at),
            None => Some(at),
        }
    }

    /// Every alternative of a conditional, in order: Python/PHP `elif`
    /// chains carry several, ending in an optional `else`.
    fn alternatives<'t>(&self, node: TsNode<'t>) -> Vec<TsNode<'t>> {
        let mut cursor = node.walk();
        for name in ALTERNATIVE_FIELDS {
            let found: Vec<_> = node.children_by_field_name(name, &mut cursor).collect();
            if !found.is_empty() {
                return found;
            }
        }
        let Some(else_kind) = self.rules.else_node else {
            return Vec::new();
        };
        node.named_children(&mut cursor)
            .filter(|child| child.kind() == else_kind)
            .collect()
    }

    // ─── Loops ───────────────────────────────────────────────────────────

    fn walk_loop(&mut self, node: TsNode<'_>, at: Cursor, conditional: bool) -> Option<Cursor> {
        let name = self.take_label(node);
        let header_label = if conditional {
            label::LOOP_HEADER
        } else {
            label::LOOP
        };
        let header = self.enter(at, header_label, CfgBlockKind::Loop, node);
        if let Some(block) = self.blocks.get_mut(header as usize) {
            block.end_line = block.start_line;
        }
        if conditional {
            self.may_throw(header);
            self.scan_header(node, header);
        }

        self.targets
            .push(JumpTarget::new(Target::Loop { header }, name));
        let into_body = if conditional {
            Cursor::edge(header, CfgEdgeKind::BranchTrue)
        } else {
            Cursor::after(header)
        };
        let body_end = match node.child_by_field_name("body") {
            Some(body) => self.walk_stmt(body, into_body),
            None => Some(into_body),
        };
        if let Some(end) = body_end {
            self.add_edge(end.from, header, CfgEdgeKind::LoopBack);
        }
        let mut ends = self.pop_breaks();

        if conditional {
            let exhausted = Cursor::edge(header, CfgEdgeKind::BranchFalse);
            match self.loop_else(node) {
                // Python `for … else:` runs when the loop ends without `break`.
                Some(alt) => {
                    let else_id = self.enter(exhausted, label::ELSE, CfgBlockKind::Normal, alt);
                    ends.extend(self.walk_stmt(alt, Cursor::after(else_id)));
                }
                None => ends.push(exhausted),
            }
        }
        self.join(ends, label::AFTER_LOOP, lines(node).1)
    }

    /// Whether a condition-style loop has a header at all; Go `for {}` and
    /// C `for (;;)` hold only their body and never exit through it.
    fn has_loop_header(&self, node: TsNode<'_>) -> bool {
        let body = node.child_by_field_name("body");
        let label_kind = self.rules.labels.label_kind();
        let mut cursor = node.walk();
        node.named_children(&mut cursor).any(|child| {
            !child.is_extra() && Some(child) != body && Some(child.kind()) != label_kind
        })
    }

    fn loop_else<'t>(&self, node: TsNode<'t>) -> Option<TsNode<'t>> {
        node.child_by_field_name("alternative")
            .filter(|alt| Some(alt.kind()) == self.rules.else_node)
    }

    // ─── Switches ────────────────────────────────────────────────────────

    fn walk_switch(&mut self, node: TsNode<'_>, at: Cursor) -> Option<Cursor> {
        let name = self.take_label(node);
        let branch = self.branch_block(at, label::MATCH, node);
        let tracks_breaks = self.rules.break_exits_switch;
        if tracks_breaks {
            self.targets.push(JumpTarget::new(Target::Switch, name));
        }

        let arms = self.switch_arms(node);
        let mut ends = Vec::new();
        let mut carry: Vec<Cursor> = Vec::new();
        let mut has_default = false;
        for &arm in &arms {
            has_default |= self.rules.is_default_arm(self.text(arm));
            let case = self.enter(
                Cursor::after(branch),
                label::CASE,
                CfgBlockKind::Normal,
                arm,
            );
            for from in carry.drain(..) {
                self.add_edge(from.from, case, from.kind);
            }
            let arm_end = self.walk_seq(arm, Cursor::after(case));
            carry.append(&mut self.fallthrough);
            match arm_end {
                Some(end) if self.rules.fallthrough_cases.contains(&arm.kind()) => carry.push(end),
                Some(end) => ends.push(end),
                None => {}
            }
        }
        // The last arm falls out of the bottom.
        ends.append(&mut carry);
        if tracks_breaks {
            ends.extend(self.pop_breaks());
        }
        let exhaustive = self.rules.default_case == SwitchDefault::Exhaustive;
        if arms.is_empty() || !(has_default || exhaustive) {
            // MIR's `otherwise`: no arm matched.
            ends.push(Cursor::edge(branch, CfgEdgeKind::BranchFalse));
        }
        self.join(ends, label::AFTER_MATCH, lines(node).1)
    }

    fn switch_arms<'t>(&self, node: TsNode<'t>) -> Vec<TsNode<'t>> {
        let container = node.child_by_field_name("body").unwrap_or(node);
        let mut cursor = container.walk();
        container
            .named_children(&mut cursor)
            .filter(|child| self.rules.case_nodes.contains(&child.kind()))
            .collect()
    }
}

/// Unwrap an `expression_statement` to the expression it holds: Rust wraps
/// `if`/`match`/`loop` used as statements, JS/Python wrap calls and
/// assignments.
fn unwrap_statement(node: TsNode<'_>) -> TsNode<'_> {
    if node.kind() == "expression_statement" {
        node.named_child(0).unwrap_or(node)
    } else {
        node
    }
}

fn field<'t>(node: TsNode<'t>, names: &[&str]) -> Option<TsNode<'t>> {
    names.iter().find_map(|name| node.child_by_field_name(name))
}

/// 1-based first and last line of `node`.
fn lines(node: TsNode<'_>) -> (u32, u32) {
    (
        node.start_position().row as u32 + 1,
        node.end_position().row as u32 + 1,
    )
}

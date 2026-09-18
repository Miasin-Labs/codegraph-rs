//! Per-function control flow graph (CFG) construction from tree-sitter ASTs.
//!
//! Builds a basic-block graph with typed edges for each function body.
//! The resulting [`FunctionCfg`] can be stored on [`crate::nodes::NodeData`]
//! and queried via the DSL `cfg` operator.
//!
//! The builder follows rustc MIR's structural invariants, checked by
//! [`FunctionCfg::validate`]: every block other than EXIT ends in a
//! terminator with a successor (a jump, a branch, or fallthrough), every
//! switch has an `otherwise` edge unless it is exhaustive or has a default
//! arm, and EXIT is reachable from ENTRY whenever the function can return.

mod builder;
mod validate;

use serde::{Deserialize, Serialize};
use tree_sitter::Node as TsNode;
pub use validate::CfgViolation;

use crate::cfg_rules::CfgRules;

// ─── Core Types ──────────────────────────────────────────────────────────────

/// A control flow graph for a single function.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionCfg {
    pub blocks: Vec<CfgBlock>,
    pub edges: Vec<CfgEdge>,
}

/// A basic block in the CFG.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CfgBlock {
    pub id: u32,
    pub label: String,
    pub start_line: u32,
    pub end_line: u32,
    pub kind: CfgBlockKind,
}

/// Classification of a basic block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CfgBlockKind {
    Entry,
    Exit,
    Normal,
    Branch,
    Loop,
    Exception,
}

/// An edge between two basic blocks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CfgEdge {
    pub from: u32,
    pub to: u32,
    pub kind: CfgEdgeKind,
}

/// Classification of a CFG edge.
///
/// A switch's "no arm matched" (`otherwise`) edge is a [`Self::BranchFalse`]
/// from the switch block; a `throw` (or a statement inside a `try` body)
/// reaches its handler — or EXIT, when uncaught — over [`Self::Exception`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CfgEdgeKind {
    Normal,
    BranchTrue,
    BranchFalse,
    LoopBack,
    Exception,
    Break,
    Continue,
    Return,
}

/// Block labels the builder emits.
mod label {
    pub(super) const ENTRY: &str = "ENTRY";
    pub(super) const EXIT: &str = "EXIT";
    pub(super) const STMT: &str = "stmt";
    pub(super) const IF: &str = "if";
    pub(super) const ELIF: &str = "elif";
    pub(super) const ELSE: &str = "else";
    pub(super) const AFTER_IF: &str = "after_if";
    pub(super) const LOOP_HEADER: &str = "loop_header";
    pub(super) const LOOP: &str = "loop";
    pub(super) const AFTER_LOOP: &str = "after_loop";
    pub(super) const MATCH: &str = "match";
    pub(super) const CASE: &str = "case";
    pub(super) const AFTER_MATCH: &str = "after_match";
    pub(super) const AFTER_LABEL: &str = "after_label";
    pub(super) const TRY: &str = "try";
    pub(super) const CATCH: &str = "catch";
    pub(super) const FINALLY: &str = "finally";
    pub(super) const AFTER_TRY: &str = "after_try";
    pub(super) const RETURN: &str = "return";
    pub(super) const THROW: &str = "throw";
    pub(super) const BREAK: &str = "break";
    pub(super) const CONTINUE: &str = "continue";
}

impl CfgBlock {
    /// Whether this block ends in an explicit jump (`return`, `throw`,
    /// `break`, `continue`) — the only non-EXIT blocks that may lack a
    /// successor, when the jump's target lies outside the modelled body
    /// (e.g. a `break` with no enclosing loop).
    pub fn is_jump(&self) -> bool {
        matches!(
            self.label.as_str(),
            label::RETURN | label::THROW | label::BREAK | label::CONTINUE
        )
    }
}

// ─── Public API ──────────────────────────────────────────────────────────────

/// Build a CFG for a function body node.
///
/// Returns `None` if no rules exist for the given language or the node has no body.
pub fn build_cfg(
    function_node: TsNode<'_>,
    source: &[u8],
    language_id: &str,
) -> Option<FunctionCfg> {
    let rules = CfgRules::for_language(language_id)?;

    // Verify this is a function node.
    if !rules.function_nodes.contains(&function_node.kind()) {
        return None;
    }

    let body = function_node.child_by_field_name(rules.body_field)?;
    let cfg = builder::build_function(function_node, body, source, rules);
    debug_assert!(
        cfg.validate().is_ok(),
        "malformed CFG for `{}` at line {}: {:?}",
        function_node.kind(),
        function_node.start_position().row + 1,
        cfg.validate()
    );
    Some(cfg)
}

// ─── Display ─────────────────────────────────────────────────────────────────

impl FunctionCfg {
    /// Format the CFG as a human-readable string for DSL output.
    pub fn format_summary(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "blocks={} edges={}\n",
            self.blocks.len(),
            self.edges.len()
        ));
        for block in &self.blocks {
            out.push_str(&format!(
                "  B{}: {:?} \"{}\" L{}-{}\n",
                block.id, block.kind, block.label, block.start_line, block.end_line
            ));
        }
        for edge in &self.edges {
            out.push_str(&format!(
                "  B{} -> B{} [{:?}]\n",
                edge.from, edge.to, edge.kind
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests;

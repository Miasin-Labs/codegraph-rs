//! Control-flow dominance over a per-function [`FunctionCfg`].
//!
//! The guard miner ([`super::mining`]) asks one question of every caller:
//! *does this guard call execute before the sink call?* The cheap answer is
//! textual — "the guard's line is smaller". That is wrong whenever the guard
//! sits inside a conditional branch: a guard in a `then`-arm does **not**
//! protect a sink that follows the `if`, because the `else`/fall-through path
//! reaches the sink without ever running the guard.
//!
//! The correct answer is **control-flow dominance**: the guard's basic block
//! must lie on *every* path from the function entry to the sink's basic block.
//! This module maps a source line to its narrowest CFG block and runs the
//! existing [`crate::dominators::Dominators`] (Cooper-Harvey-Kennedy) over the
//! function's basic-block graph to decide it. When the CFG is unavailable the
//! miner falls back to line ordering, so this only ever *sharpens* results.

use petgraph::graph::{DiGraph, NodeIndex};

use crate::cfg::FunctionCfg;
use crate::dominators::Dominators;

/// The CFG block whose line span most tightly encloses `line`.
///
/// Blocks overlap (an `if` branch block spans its whole body, while the
/// statement blocks inside it span single statements). We want the *narrowest*
/// enclosing block — the one that actually represents the statement at `line` —
/// so ties on width break toward the higher (more nested, later-built) id.
/// Returns `None` when no block covers the line (e.g. a line outside the body).
pub fn block_containing_line(cfg: &FunctionCfg, line: u32) -> Option<u32> {
    cfg.blocks
        .iter()
        .filter(|b| b.start_line <= line && line <= b.end_line)
        .min_by_key(|b| (b.end_line - b.start_line, u32::MAX - b.id))
        .map(|b| b.id)
}

/// Does the statement at `dominator_line` dominate the statement at
/// `dominated_line` in this function's control flow?
///
/// `Some(true)`  — the dominator block lies on every path from entry to the
///                 dominated block (so the call at `dominator_line` always runs
///                 before the call at `dominated_line`).
/// `Some(false)` — there is a path to the dominated block that skips it.
/// `None`        — a line could not be mapped to a block, or the CFG is empty;
///                 the caller should fall back to a textual heuristic.
///
/// Two calls in the same basic block are sequential, so dominance reduces to
/// textual order within that block.
pub fn dominates_by_line(
    cfg: &FunctionCfg,
    dominator_line: u32,
    dominated_line: u32,
) -> Option<bool> {
    if cfg.blocks.is_empty() {
        return None;
    }
    let dom_block = block_containing_line(cfg, dominator_line)?;
    let sub_block = block_containing_line(cfg, dominated_line)?;
    if dom_block == sub_block {
        return Some(dominator_line <= dominated_line);
    }

    // Block ids are assigned sequentially from 0 (ENTRY), so block `i` maps to
    // `NodeIndex::new(i)`. Materialise the CFG as a petgraph DiGraph and run the
    // shared dominator algorithm rooted at the entry block.
    let mut g: DiGraph<(), ()> = DiGraph::with_capacity(cfg.blocks.len(), cfg.edges.len());
    for _ in &cfg.blocks {
        g.add_node(());
    }
    let n = cfg.blocks.len();
    for e in &cfg.edges {
        if (e.from as usize) < n && (e.to as usize) < n {
            g.add_edge(
                NodeIndex::new(e.from as usize),
                NodeIndex::new(e.to as usize),
                (),
            );
        }
    }

    let dom = Dominators::build(&g, NodeIndex::new(0));
    Some(dom.dominates(
        &NodeIndex::new(dom_block as usize),
        &NodeIndex::new(sub_block as usize),
    ))
}

#[cfg(test)]
mod tests {
    use tree_sitter::{Node as TsNode, Parser};

    use super::*;
    use crate::cfg::{CfgBlock, CfgBlockKind, CfgEdge, CfgEdgeKind, build_cfg};

    fn rust_cfg(src: &str) -> FunctionCfg {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(src, None).unwrap();
        let root = tree.root_node();
        let mut cursor = root.walk();
        let func: TsNode<'_> = root
            .named_children(&mut cursor)
            .find(|c| c.kind() == "function_item")
            .expect("no function_item");
        build_cfg(func, src.as_bytes(), "rust").expect("cfg")
    }

    /// 1-based line of the first occurrence of `needle` in `src`.
    fn line_of(src: &str, needle: &str) -> u32 {
        let idx = src.find(needle).expect("needle not found");
        (src[..idx].matches('\n').count() + 1) as u32
    }

    #[test]
    fn straight_line_call_dominates_later_call() {
        let src = "fn h() {\n    guard_call();\n    sink_call();\n}\n";
        let cfg = rust_cfg(src);
        let g = line_of(src, "guard_call");
        let s = line_of(src, "sink_call");
        assert_eq!(dominates_by_line(&cfg, g, s), Some(true));
        // …and not the other way around: the sink does not run before the guard
        // (same basic block ⇒ dominance reduces to textual order).
        assert_eq!(dominates_by_line(&cfg, s, g), Some(false));
    }

    #[test]
    fn guard_inside_if_does_not_dominate_sink_after_if() {
        // The whole point: a guard in the `then` arm does NOT protect a sink
        // that follows the `if`, because the fall-through path skips it.
        let src = "fn h(cond: bool) {\n    if cond {\n        guard_call();\n    }\n    sink_call();\n}\n";
        let cfg = rust_cfg(src);
        let g = line_of(src, "guard_call");
        let s = line_of(src, "sink_call");
        assert_eq!(
            dominates_by_line(&cfg, g, s),
            Some(false),
            "guard in a conditional branch must not dominate a sink after the branch\n{}",
            cfg.format_summary()
        );
    }

    #[test]
    fn guard_before_if_dominates_sink_inside_if() {
        let src = "fn h(cond: bool) {\n    guard_call();\n    if cond {\n        sink_call();\n    }\n}\n";
        let cfg = rust_cfg(src);
        let g = line_of(src, "guard_call");
        let s = line_of(src, "sink_call");
        assert_eq!(
            dominates_by_line(&cfg, g, s),
            Some(true),
            "{}",
            cfg.format_summary()
        );
    }

    #[test]
    fn unmapped_line_returns_none() {
        let cfg = FunctionCfg {
            blocks: vec![CfgBlock {
                id: 0,
                label: "ENTRY".into(),
                start_line: 1,
                end_line: 1,
                kind: CfgBlockKind::Entry,
            }],
            edges: vec![],
        };
        assert_eq!(dominates_by_line(&cfg, 99, 1), None);
        assert_eq!(
            dominates_by_line(
                &FunctionCfg {
                    blocks: vec![],
                    edges: vec![]
                },
                1,
                1
            ),
            None
        );
    }

    #[test]
    fn narrowest_block_wins_over_enclosing_branch() {
        // A wide branch block [2..6] and a narrow stmt block [4..4] both cover
        // line 4; the narrow one is the real statement.
        let cfg = FunctionCfg {
            blocks: vec![
                CfgBlock {
                    id: 0,
                    label: "ENTRY".into(),
                    start_line: 1,
                    end_line: 1,
                    kind: CfgBlockKind::Entry,
                },
                CfgBlock {
                    id: 1,
                    label: "if".into(),
                    start_line: 2,
                    end_line: 6,
                    kind: CfgBlockKind::Branch,
                },
                CfgBlock {
                    id: 2,
                    label: "stmt".into(),
                    start_line: 4,
                    end_line: 4,
                    kind: CfgBlockKind::Normal,
                },
            ],
            edges: vec![
                CfgEdge {
                    from: 0,
                    to: 1,
                    kind: CfgEdgeKind::Normal,
                },
                CfgEdge {
                    from: 1,
                    to: 2,
                    kind: CfgEdgeKind::BranchTrue,
                },
            ],
        };
        assert_eq!(block_containing_line(&cfg, 4), Some(2));
    }
}

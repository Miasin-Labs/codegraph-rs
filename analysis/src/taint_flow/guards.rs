//! Validation guards as sanitizers: `if (x < MAX) { sink(x + 1) }`,
//! `if (is_numeric($x)) { query($x) }`.
//!
//! A rule's guard marks a check (the condition expression) and the value it
//! checks, and says on which branch the value is safe. A read of that value
//! is clean where the safe edge of a branch on the check dominates it and
//! the value is still the one checked (no definition of it reaches the read
//! that did not reach the check). Dominance is over the basic blocks
//! reaching definitions solve on, so pruned constant branches stay pruned.

use crate::ir::{BinOpKind, IrFunction, IrOp, Operand, Place, Var};
use crate::reaching_defs::{Blocks, DefId, ReachingDefs, is_temp};

/// Immediate dominators of the reachable blocks (Cooper–Harvey–Kennedy).
pub(super) struct Dominators {
    idom: Vec<Option<usize>>,
}

impl Dominators {
    pub fn compute(blocks: &Blocks) -> Self {
        let count = blocks.len();
        // Reverse postorder from the entry, iteratively.
        let mut order: Vec<usize> = Vec::with_capacity(count);
        let mut visited = vec![false; count];
        if count > 0 {
            let mut stack: Vec<(usize, usize)> = vec![(0, 0)];
            visited[0] = true;
            while let Some((block, next)) = stack.pop() {
                let succs = blocks.succs(block);
                if next < succs.len() {
                    stack.push((block, next + 1));
                    let succ = succs[next];
                    if !visited[succ] {
                        visited[succ] = true;
                        stack.push((succ, 0));
                    }
                } else {
                    order.push(block);
                }
            }
        }
        order.reverse();
        let mut rank = vec![usize::MAX; count];
        for (index, &block) in order.iter().enumerate() {
            rank[block] = index;
        }
        let mut idom: Vec<Option<usize>> = vec![None; count];
        if count == 0 {
            return Self { idom };
        }
        idom[0] = Some(0);
        let mut changed = true;
        while changed {
            changed = false;
            for &block in order.iter().skip(1) {
                let mut new: Option<usize> = None;
                for &pred in blocks.preds(block) {
                    if idom[pred].is_none() {
                        continue;
                    }
                    new = Some(match new {
                        None => pred,
                        Some(current) => intersect(&idom, &rank, pred, current),
                    });
                }
                if new.is_some() && idom[block] != new {
                    idom[block] = new;
                    changed = true;
                }
            }
        }
        Self { idom }
    }

    /// Whether block `a` dominates block `b` (every block dominates itself).
    pub fn dominates(&self, a: usize, b: usize) -> bool {
        let mut current = b;
        // The chain to the entry is at most the block count long.
        for _ in 0..=self.idom.len() {
            if current == a {
                return true;
            }
            match self.idom.get(current).copied().flatten() {
                Some(parent) if parent != current => current = parent,
                _ => return false,
            }
        }
        false
    }
}

fn intersect(idom: &[Option<usize>], rank: &[usize], mut a: usize, mut b: usize) -> usize {
    while a != b {
        while rank[a] > rank[b] {
            a = idom[a].unwrap_or(0);
        }
        while rank[b] > rank[a] {
            b = idom[b].unwrap_or(0);
        }
    }
    a
}

/// A resolved guard: reads of `place` in blocks `safe` dominates are clean
/// while they see only definitions from `checked`.
#[derive(Debug, Clone)]
pub(super) struct Guard {
    pub place: Place,
    pub safe: usize,
    pub checked: Vec<DefId>,
}

/// The branches whose condition is `check` (possibly inside `&&` or
/// negated), and on which edge the check holds: `(branch op, holds when
/// taken)`.
pub(super) fn branches_on(func: &IrFunction, check: &Operand) -> Vec<(usize, bool)> {
    let Operand::Var(check) = check else {
        return Vec::new();
    };
    let mut defs: std::collections::HashMap<&Var, usize> = std::collections::HashMap::new();
    for (index, op) in func.body.iter().enumerate() {
        if let IrOp::Assign { dst, .. } | IrOp::BinOp { dst, .. } = op {
            if is_temp(dst) {
                defs.insert(dst, index);
            }
        }
    }
    let mut found = Vec::new();
    for (index, op) in func.body.iter().enumerate() {
        let IrOp::Branch {
            cond: Operand::Var(cond),
            ..
        } = op
        else {
            continue;
        };
        // Walk down from the condition: through `&&` (both sides hold when
        // it does), negation (flips), and copies.
        let mut stack: Vec<(&Var, bool, usize)> = vec![(cond, true, 0)];
        while let Some((var, holds, depth)) = stack.pop() {
            if var == check {
                found.push((index, holds));
                continue;
            }
            if depth > 16 {
                continue;
            }
            let Some(&def) = defs.get(var) else {
                continue;
            };
            match &func.body[def] {
                IrOp::Assign {
                    src: Operand::Var(src),
                    ..
                } => stack.push((src, holds, depth + 1)),
                IrOp::BinOp {
                    lhs,
                    op: BinOpKind::And,
                    rhs,
                    ..
                } if holds => {
                    for side in [lhs, rhs] {
                        if let Operand::Var(side) = side {
                            stack.push((side, true, depth + 1));
                        }
                    }
                }
                // `!x` lowers to `x == false`.
                IrOp::BinOp {
                    lhs: Operand::Var(inner),
                    op: BinOpKind::Eq,
                    rhs: Operand::Const(c),
                    ..
                }
                | IrOp::BinOp {
                    lhs: Operand::Const(c),
                    op: BinOpKind::Eq,
                    rhs: Operand::Var(inner),
                    ..
                } if c == "false" => stack.push((inner, !holds, depth + 1)),
                _ => {}
            }
        }
    }
    found
}

/// Resolve a guard on `place` checked by `check` (safe on the edge where
/// the check holds when `safe_when_true`, else where it fails).
pub(super) fn resolve(
    func: &IrFunction,
    rd: &ReachingDefs,
    check: &Operand,
    place: &Place,
    safe_when_true: bool,
) -> Vec<Guard> {
    let blocks = &rd.blocks;
    let mut guards = Vec::new();
    for (branch, holds_when_taken) in branches_on(func, check) {
        let Some(block) = blocks.block_of(branch) else {
            continue;
        };
        let IrOp::Branch { target, .. } = &func.body[branch] else {
            continue;
        };
        let taken = func.labels.get(target).and_then(|&op| blocks.block_of(op));
        let fallthrough = (block + 1 < blocks.len()).then_some(block + 1);
        let safe = if holds_when_taken == safe_when_true {
            taken
        } else {
            fallthrough
        };
        // The edge dominates what its target dominates only when it is
        // the target's one way in.
        let Some(safe) = safe.filter(|&safe| blocks.preds(safe) == [block]) else {
            continue;
        };
        let checked = rd.reaching(branch, place);
        guards.push(Guard {
            place: place.clone(),
            safe,
            checked,
        });
    }
    guards
}

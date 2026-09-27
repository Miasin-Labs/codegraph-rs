//! Basic blocks of an [`IrFunction`]: leaders are the first op, every
//! label, and every op after a branch, jump or return. `Branch` goes to
//! its target and falls through; a branch on a constant condition keeps
//! only the edge it takes.

use std::collections::HashMap;
use std::ops::Range;

use crate::ir::{IrFunction, IrOp};

pub struct Blocks {
    /// `starts[b]..starts[b + 1]` (or the body's end) are block `b`'s ops.
    starts: Vec<usize>,
    len: usize,
    succs: Vec<Vec<usize>>,
    preds: Vec<Vec<usize>>,
    reachable: Vec<bool>,
}

impl Blocks {
    /// `constants`: op index of a `Branch` → whether it is always taken.
    pub fn build(func: &IrFunction, constants: &HashMap<usize, bool>) -> Self {
        let body = &func.body;
        let mut is_leader = vec![false; body.len()];
        if let Some(first) = is_leader.first_mut() {
            *first = true;
        }
        for (index, op) in body.iter().enumerate() {
            match op {
                IrOp::Label(_) => is_leader[index] = true,
                IrOp::Branch { .. } | IrOp::Jump { .. } | IrOp::Return { .. } => {
                    if let Some(next) = is_leader.get_mut(index + 1) {
                        *next = true;
                    }
                }
                _ => {}
            }
        }
        let starts: Vec<usize> = (0..body.len()).filter(|&i| is_leader[i]).collect();
        let count = starts.len();
        let mut blocks = Self {
            starts,
            len: body.len(),
            succs: vec![Vec::new(); count],
            preds: vec![Vec::new(); count],
            reachable: vec![false; count],
        };
        let target_block = |label| {
            func.labels
                .get(&label)
                .and_then(|&index| blocks.block_of(index))
        };
        let mut edges: Vec<(usize, usize)> = Vec::new();
        for block in 0..count {
            let range = blocks.range(block);
            let last = range.end - 1;
            let fallthrough = (block + 1 < count).then_some(block + 1);
            match &body[last] {
                IrOp::Jump { target } => edges.extend(target_block(*target).map(|to| (block, to))),
                IrOp::Branch { target, .. } => {
                    let taken = constants.get(&last).copied();
                    if taken != Some(false) {
                        edges.extend(target_block(*target).map(|to| (block, to)));
                    }
                    if taken != Some(true) {
                        edges.extend(fallthrough.map(|to| (block, to)));
                    }
                }
                IrOp::Return { .. } => {}
                _ => edges.extend(fallthrough.map(|to| (block, to))),
            }
        }
        for (from, to) in edges {
            if !blocks.succs[from].contains(&to) {
                blocks.succs[from].push(to);
                blocks.preds[to].push(from);
            }
        }
        // Reachability from the entry block.
        let mut stack: Vec<usize> = if count > 0 { vec![0] } else { Vec::new() };
        while let Some(block) = stack.pop() {
            if std::mem::replace(&mut blocks.reachable[block], true) {
                continue;
            }
            stack.extend(blocks.succs[block].iter().copied());
        }
        blocks
    }

    pub fn len(&self) -> usize {
        self.starts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.starts.is_empty()
    }

    /// The op indices of `block`.
    pub fn range(&self, block: usize) -> Range<usize> {
        let end = self.starts.get(block + 1).copied().unwrap_or(self.len);
        self.starts[block]..end
    }

    /// The block holding op `op`.
    pub fn block_of(&self, op: usize) -> Option<usize> {
        if op >= self.len {
            return None;
        }
        match self.starts.binary_search(&op) {
            Ok(block) => Some(block),
            Err(next) => next.checked_sub(1),
        }
    }

    pub fn succs(&self, block: usize) -> &[usize] {
        &self.succs[block]
    }

    pub fn preds(&self, block: usize) -> &[usize] {
        &self.preds[block]
    }

    pub fn reachable(&self, block: usize) -> bool {
        self.reachable[block]
    }
}

//! Structural invariants of a [`FunctionCfg`], after rustc MIR's.

use std::collections::VecDeque;

use super::{CfgBlockKind, FunctionCfg};

/// A structural invariant a [`FunctionCfg`] breaks.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CfgViolation {
    /// Blocks 0 and 1 must be ENTRY and EXIT.
    #[error("blocks 0 and 1 must be ENTRY and EXIT")]
    MissingEntryOrExit,
    /// Every block's id must equal its index in `blocks`.
    #[error("block at index {index} has id {id}")]
    BlockIdMismatch { index: usize, id: u32 },
    /// An edge names a block that does not exist.
    #[error("edge B{from} -> B{to} names a block that does not exist")]
    DanglingEdge { from: u32, to: u32 },
    /// A non-EXIT block has no successor and does not end in a jump.
    #[error("block B{block} (\"{label}\") has no successor and does not end in a jump")]
    MissingTerminator { block: u32, label: String },
    /// Some block reaches EXIT (the function can terminate), yet no path
    /// from ENTRY does.
    #[error("EXIT has predecessors but is unreachable from ENTRY")]
    ExitUnreachable,
}

const ENTRY_ID: usize = 0;
const EXIT_ID: usize = 1;

impl FunctionCfg {
    /// Check the builder's structural invariants:
    ///
    /// - blocks 0/1 are ENTRY/EXIT, ids equal indices, edges name real blocks;
    /// - every non-EXIT block without a successor ends in a jump (`return`,
    ///   `throw`, `break`, `continue`) whose target lies outside the body;
    /// - EXIT is reachable from ENTRY whenever the function can terminate
    ///   (some edge enters EXIT; a function that only loops forever has none).
    pub fn validate(&self) -> Result<(), CfgViolation> {
        self.check_blocks()?;
        let successors = self.successors()?;
        self.check_terminators(&successors)?;
        self.check_exit_reachable(&successors)
    }

    fn check_blocks(&self) -> Result<(), CfgViolation> {
        let kind_at = |index: usize| self.blocks.get(index).map(|block| block.kind);
        if kind_at(ENTRY_ID) != Some(CfgBlockKind::Entry)
            || kind_at(EXIT_ID) != Some(CfgBlockKind::Exit)
        {
            return Err(CfgViolation::MissingEntryOrExit);
        }
        match self
            .blocks
            .iter()
            .enumerate()
            .find(|(index, block)| block.id as usize != *index)
        {
            Some((index, block)) => Err(CfgViolation::BlockIdMismatch {
                index,
                id: block.id,
            }),
            None => Ok(()),
        }
    }

    /// Successor lists by block index.
    fn successors(&self) -> Result<Vec<Vec<usize>>, CfgViolation> {
        let mut successors = vec![Vec::new(); self.blocks.len()];
        for edge in &self.edges {
            let (from, to) = (edge.from as usize, edge.to as usize);
            if from >= self.blocks.len() || to >= self.blocks.len() {
                return Err(CfgViolation::DanglingEdge {
                    from: edge.from,
                    to: edge.to,
                });
            }
            successors[from].push(to);
        }
        Ok(successors)
    }

    fn check_terminators(&self, successors: &[Vec<usize>]) -> Result<(), CfgViolation> {
        let unterminated = self.blocks.iter().zip(successors).find(|(block, succ)| {
            succ.is_empty() && block.kind != CfgBlockKind::Exit && !block.is_jump()
        });
        match unterminated {
            Some((block, _)) => Err(CfgViolation::MissingTerminator {
                block: block.id,
                label: block.label.clone(),
            }),
            None => Ok(()),
        }
    }

    fn check_exit_reachable(&self, successors: &[Vec<usize>]) -> Result<(), CfgViolation> {
        let can_terminate = successors.iter().flatten().any(|&to| to == EXIT_ID);
        if !can_terminate || reachable_from(ENTRY_ID, successors)[EXIT_ID] {
            Ok(())
        } else {
            Err(CfgViolation::ExitUnreachable)
        }
    }
}

/// Which blocks a breadth-first walk from `start` visits.
fn reachable_from(start: usize, successors: &[Vec<usize>]) -> Vec<bool> {
    let mut seen = vec![false; successors.len()];
    let mut queue = VecDeque::from([start]);
    seen[start] = true;
    while let Some(block) = queue.pop_front() {
        for &next in &successors[block] {
            if !seen[next] {
                seen[next] = true;
                queue.push_back(next);
            }
        }
    }
    seen
}

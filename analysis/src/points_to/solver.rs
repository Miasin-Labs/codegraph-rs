//! The intraprocedural fixpoint solver: evaluate every op once, then re-run
//! only the ops whose inputs grew, until none did.

use std::collections::{BTreeSet, HashMap};

use super::worklist::DirtySet;
use super::{AbstractLocation, MAX_FIELD_DEPTH, PointsToTable, field_cell, operand_var};
use crate::ir::{IrFunction, IrOp, Operand, Var};
use crate::nodes::NodeId;

/// Which ops must re-run when a points-to set grows.
#[derive(Debug, Default)]
struct UseIndex {
    /// Ops that read `pts(var)`.
    var_readers: HashMap<Var, Vec<usize>>,
    /// `FieldRead` ops by field name: a read of `f` looks up `Field(_, f)`.
    field_readers: HashMap<String, Vec<usize>>,
    /// Every `FieldRead` op: any of them can land on a k-limit summary cell.
    all_field_readers: Vec<usize>,
}

impl UseIndex {
    fn build(ir: &IrFunction) -> Self {
        let mut index = Self::default();
        for (idx, op) in ir.body.iter().enumerate() {
            match op {
                IrOp::Assign { src, .. } => index.reads(src, idx),
                IrOp::FieldRead { base, field, .. } => {
                    index.reads(base, idx);
                    index
                        .field_readers
                        .entry(field.clone())
                        .or_default()
                        .push(idx);
                    index.all_field_readers.push(idx);
                }
                IrOp::FieldWrite { base, src, .. } => {
                    index.reads(base, idx);
                    index.reads(src, idx);
                }
                IrOp::BinOp { .. }
                | IrOp::Call { .. }
                | IrOp::Branch { .. }
                | IrOp::Jump { .. }
                | IrOp::Return { .. }
                | IrOp::Label(_)
                | IrOp::Nop => {}
            }
        }
        index
    }

    fn reads(&mut self, operand: &Operand, idx: usize) {
        if let Some(var) = operand_var(operand) {
            self.var_readers
                .entry(var.into_owned())
                .or_default()
                .push(idx);
        }
    }

    fn readers_of_var(&self, var: &Var) -> &[usize] {
        self.var_readers.get(var).map_or(&[], Vec::as_slice)
    }

    /// Reads that can look up `cell`: those of its field name, or — for a
    /// summary cell at the depth limit, which is its own field — all of them.
    fn readers_of_cell(&self, cell: &AbstractLocation) -> &[usize] {
        match cell {
            AbstractLocation::Field(_, name) if cell.depth() < MAX_FIELD_DEPTH => {
                self.field_readers.get(name).map_or(&[], Vec::as_slice)
            }
            _ => &self.all_field_readers,
        }
    }
}

/// How many more facts the solve may add; see [`super::FACT_BUDGET`].
#[derive(Debug)]
struct Budget {
    remaining: usize,
    exhausted: bool,
}

impl Budget {
    /// Pay for one new fact; `false` (and exhausted) once none is left.
    fn spend(&mut self) -> bool {
        if self.remaining == 0 {
            self.exhausted = true;
            return false;
        }
        self.remaining -= 1;
        true
    }
}

/// Solves one function's constraints (see the module docs) to their least
/// fixpoint. Interprocedural propagation adds [`Self::seed`]s and re-solves;
/// only the ops that read a seeded variable re-run.
pub(super) struct FunctionSolver<'ir> {
    owner: NodeId,
    ir: &'ir IrFunction,
    index: UseIndex,
    dirty: DirtySet,
    table: PointsToTable,
    budget: Budget,
}

impl<'ir> FunctionSolver<'ir> {
    /// A solver with each parameter pointing to its own [`AbstractLocation::Param`]
    /// and every op pending. `budget` caps the facts it may add.
    pub(super) fn new(owner: NodeId, ir: &'ir IrFunction, budget: usize) -> Self {
        let mut solver = Self {
            index: UseIndex::build(ir),
            dirty: DirtySet::all(ir.body.len()),
            table: PointsToTable::new(),
            budget: Budget {
                remaining: budget,
                exhausted: false,
            },
            owner,
            ir,
        };
        for param in &ir.params {
            let loc = AbstractLocation::param(solver.owner.clone(), param.as_str());
            solver.add_to_var(param, [loc]);
        }
        solver
    }

    pub(super) fn ir(&self) -> &'ir IrFunction {
        self.ir
    }

    pub(super) fn table(&self) -> &PointsToTable {
        &self.table
    }

    /// The solved table; `converged` is `false` iff the budget ran out.
    pub(super) fn into_table(mut self) -> PointsToTable {
        self.table.converged = !self.budget.exhausted;
        self.table
    }

    /// `pts(var) ⊇ locs` from outside the body (interprocedural flow).
    /// Returns `true` if the set grew; [`Self::solve`] then re-runs its readers.
    pub(super) fn seed(&mut self, var: &Var, locs: &BTreeSet<AbstractLocation>) -> bool {
        self.add_to_var(var, locs.iter().cloned())
    }

    /// Evaluate pending ops until none is left — the fixpoint.
    pub(super) fn solve(&mut self) {
        while let Some(idx) = self.dirty.pop() {
            self.eval(idx);
        }
    }

    fn eval(&mut self, idx: usize) {
        let ir = self.ir;
        match &ir.body[idx] {
            IrOp::Assign {
                dst,
                src: Operand::Const(_),
            }
            | IrOp::BinOp { dst, .. } => {
                let literal = AbstractLocation::literal(self.owner.clone(), idx);
                self.add_to_var(dst, [literal]);
            }
            IrOp::Assign { dst, src } => {
                let locs = self.operand_pts(src);
                self.add_to_var(dst, locs);
            }
            IrOp::Call { dst: Some(dst), .. } => {
                let heap = AbstractLocation::heap(self.owner.clone(), idx);
                self.add_to_var(dst, [heap]);
            }
            IrOp::FieldRead { dst, base, field } => self.eval_field_read(dst, base, field),
            IrOp::FieldWrite { base, field, src } => self.eval_field_write(idx, base, field, src),
            IrOp::Call { dst: None, .. }
            | IrOp::Branch { .. }
            | IrOp::Jump { .. }
            | IrOp::Return { .. }
            | IrOp::Label(_)
            | IrOp::Nop => {}
        }
    }

    /// `dst = base.field`: each cell, plus whatever was stored into it.
    fn eval_field_read(&mut self, dst: &Var, base: &Operand, field: &str) {
        let mut locs = Vec::new();
        for base_loc in self.operand_pts(base) {
            let cell = field_cell(&base_loc, field);
            if let Some(stored) = self.table.fields.get(&cell) {
                locs.extend(stored.iter().cloned());
            }
            locs.push(cell);
        }
        self.add_to_var(dst, locs);
    }

    /// `base.field = src`: a constant source is the literal made by op `idx`.
    fn eval_field_write(&mut self, idx: usize, base: &Operand, field: &str, src: &Operand) {
        let values = match src {
            Operand::Const(_) => vec![AbstractLocation::literal(self.owner.clone(), idx)],
            Operand::Var(_) | Operand::Temp(_) => self.operand_pts(src),
        };
        if values.is_empty() {
            return;
        }
        for base_loc in self.operand_pts(base) {
            let cell = field_cell(&base_loc, field);
            self.add_to_cell(cell, values.iter().cloned());
        }
    }

    /// A snapshot of `pts(operand)`; constants point nowhere.
    fn operand_pts(&self, operand: &Operand) -> Vec<AbstractLocation> {
        operand_var(operand)
            .and_then(|var| self.table.vars.get(var.as_ref()))
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// `pts(var) ⊇ locs`; on growth, queue the ops that read `var`.
    fn add_to_var(&mut self, var: &Var, locs: impl IntoIterator<Item = AbstractLocation>) -> bool {
        let set = self.table.vars.entry(var.clone()).or_default();
        let grew = absorb(set, locs, &mut self.budget);
        if grew {
            for &op in self.index.readers_of_var(var) {
                self.dirty.push(op);
            }
        }
        grew
    }

    /// `pts(cell) ⊇ locs`; on growth, queue the reads that can see `cell`.
    fn add_to_cell(
        &mut self,
        cell: AbstractLocation,
        locs: impl IntoIterator<Item = AbstractLocation>,
    ) {
        let readers = self.index.readers_of_cell(&cell);
        let set = self.table.fields.entry(cell).or_default();
        if absorb(set, locs, &mut self.budget) {
            for &op in readers {
                self.dirty.push(op);
            }
        }
    }
}

/// Insert `locs` into `set`, paying one unit of `budget` per new location.
/// Returns `true` if the set grew.
fn absorb(
    set: &mut BTreeSet<AbstractLocation>,
    locs: impl IntoIterator<Item = AbstractLocation>,
    budget: &mut Budget,
) -> bool {
    let mut grew = false;
    for loc in locs {
        if set.contains(&loc) {
            continue;
        }
        if !budget.spend() {
            break;
        }
        set.insert(loc);
        grew = true;
    }
    grew
}

//! Field-sensitive, flow-insensitive points-to analysis (Andersen-style).
//!
//! Operates over the language-agnostic IR ([`crate::ir::IrFunction`]) emitted
//! by each adapter's [`crate::ir::IrLowering`] driver. For each named source
//! variable in a function body, computes the set of [`AbstractLocation`]s it
//! may point to.
//!
//! # Why field-sensitive?
//!
//! Coarse pointer analysis collapses every struct field into a single
//! "heap" abstraction, which destroys precision for downstream taint /
//! slicing. By distinguishing `obj.x` from `obj.y` we can answer "could
//! tainted data reach `db.execute(s.query)`?" without first sanitizing
//! the entire `s` struct.
//!
//! # Algorithm
//!
//! Each [`crate::ir::IrOp`] is interpreted as one of the classic Andersen
//! constraints (`op` is the op's index in the body, `cell` is
//! `Field(l, field)`):
//!
//! | IR shape                                | Constraint                                                   |
//! |-----------------------------------------|--------------------------------------------------------------|
//! | `dst = call(...)`                       | `pts(dst) ∋ Heap(op)` (allocation site)                      |
//! | `dst = literal` / `dst = a <op> b`      | `pts(dst) ∋ Literal(op)`                                     |
//! | `dst = src`     (`src`: var or temp)    | `pts(dst) ⊇ pts(src)`                                        |
//! | `dst = base.field`                      | for each `l ∈ pts(base)`: `pts(dst) ⊇ { cell } ∪ pts(cell)`  |
//! | `base.field = src`                      | for each `l ∈ pts(base)`: `pts(cell) ⊇ pts(src)`             |
//!
//! A field read yields the cell itself as well as everything stored into it:
//! the cell stands for the field's contents on entry (set by a caller, a
//! constructor, or code with no IR), so two reads of `p.f` alias even when
//! the body never writes `p.f`.
//!
//! The solver evaluates every op once, then re-evaluates an op only when a
//! set it reads grows: a use index maps each variable to the ops that read
//! it and each field name to the reads of that field, and a deduplicating
//! dirty set holds the ops waiting to re-run. Every constraint is monotone
//! over a finite universe of locations, so the iteration ends at the least
//! fixpoint — the same one whatever the order of the ops in the body.
//!
//! Flow-insensitive: ordering of statements is ignored, so the resulting
//! pts-sets are an over-approximation. This is the standard, well-studied
//! soundness/precision tradeoff used by LLVM's basic-aa and SVF's Andersen
//! solver — good enough for "could X alias Y" questions, less precise than
//! a sparse flow-sensitive pass.
//!
//! # Location identity
//!
//! Parameters, allocation sites and literals are named relative to one
//! function body: two functions can each have a parameter `x` or an
//! allocation at op 3. Every root location therefore carries the function
//! that owns it (its graph [`NodeId`]), so results of different functions
//! only share a location when it really flowed from one to the other.
//!
//! # Interprocedural mode
//!
//! [`analyze_interprocedural`] propagates caller argument pts-sets into
//! callee parameters (and callee return values back to caller call-site
//! destinations) across `Calls` edges, re-solving a function whenever its
//! seeds grow, until nothing grows.
//!
//! # Limitations
//!
//! - Branch- and loop-insensitive — all paths are merged.
//! - Access paths are k-limited: a cell [`MAX_FIELD_DEPTH`] fields deep
//!   summarises every longer path below it, which keeps the universe finite
//!   on cyclic structures (`node = node.next`).
//! - Each function's solve may add at most [`FACT_BUDGET`] facts. Real code
//!   stays far below it; on adversarial input the solve stops early and
//!   clears [`PointsToTable::converged`] rather than truncating silently.

mod interprocedural;
mod solver;
mod worklist;

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

pub use interprocedural::analyze_interprocedural;
use solver::FunctionSolver;

use crate::ir::{IrFunction, Operand, Var};
use crate::nodes::{NodeId, NodeKind};

/// Maximum nesting depth of `Field(Field(...))` locations. A cell this deep
/// is its own field: it summarises every longer access path below it, so
/// cyclic stores and traversals (`node = node.next`) stay finite. Kept small
/// because a loop walking `F` fields reaches every path up to this depth —
/// `Σ F^i` locations: a 4-field walk yields 341 at depth 4 (≈2 ms) but
/// 87,381 at depth 8 (≈7 s).
const MAX_FIELD_DEPTH: usize = 4;

/// Most facts (one location in one pts-set) a single function's solve may
/// add. Orders of magnitude above real functions; it only bounds work on
/// adversarial input, and hitting it is reported via
/// [`PointsToTable::converged`].
const FACT_BUDGET: usize = 1 << 20;

/// An abstract memory location that a variable can point to.
///
/// Root locations (`Heap`, `Param`, `Literal`) are tagged with the function
/// that owns them — see the module docs on location identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AbstractLocation {
    /// Allocation site: the call at op index `site` in `owner`'s body.
    Heap { owner: NodeId, site: usize },
    /// Parameter `name` of `owner` — its initial pts-set is `{ Param }` so
    /// callers can later wire interprocedural flow.
    Param { owner: NodeId, name: String },
    /// Anonymous value — a literal or an operator result — produced by op
    /// index `site` in `owner`'s body.
    Literal { owner: NodeId, site: usize },
    /// `Field(base, field_name)` — the storage cell for `base.field`.
    Field(Box<AbstractLocation>, String),
}

impl AbstractLocation {
    pub fn heap(owner: NodeId, site: usize) -> Self {
        AbstractLocation::Heap { owner, site }
    }

    pub fn param(owner: NodeId, name: impl Into<String>) -> Self {
        AbstractLocation::Param {
            owner,
            name: name.into(),
        }
    }

    pub fn literal(owner: NodeId, site: usize) -> Self {
        AbstractLocation::Literal { owner, site }
    }

    pub fn field(base: AbstractLocation, name: impl Into<String>) -> Self {
        AbstractLocation::Field(Box::new(base), name.into())
    }

    /// `Field(Field(Field(...)))` nesting depth; roots are depth 0.
    fn depth(&self) -> usize {
        let mut depth = 0;
        let mut loc = self;
        while let AbstractLocation::Field(inner, _) = loc {
            depth += 1;
            loc = inner;
        }
        depth
    }
}

/// The cell holding `base.field`, k-limited by [`MAX_FIELD_DEPTH`]: a base
/// already at the limit is its own field.
fn field_cell(base: &AbstractLocation, field: &str) -> AbstractLocation {
    if base.depth() >= MAX_FIELD_DEPTH {
        base.clone()
    } else {
        AbstractLocation::field(base.clone(), field)
    }
}

/// The variable an operand names: itself, or `__t{n}` for temp `n` (the name
/// the IR lowering gives temps). Constants name none.
fn operand_var(operand: &Operand) -> Option<Cow<'_, Var>> {
    match operand {
        Operand::Var(v) => Some(Cow::Borrowed(v)),
        Operand::Temp(t) => Some(Cow::Owned(Var::new(format!("__t{t}")))),
        Operand::Const(_) => None,
    }
}

/// Map from variable to the set of [`AbstractLocation`]s it may point to,
/// plus an auxiliary map from `Field` locations to their pts-sets (for
/// `o.f` storage cells).
#[derive(Debug, Clone)]
pub struct PointsToTable {
    /// `pts(v)` for every source variable.
    pub vars: BTreeMap<Var, BTreeSet<AbstractLocation>>,
    /// `pts(Field(...))` for every observed field cell.
    pub fields: BTreeMap<AbstractLocation, BTreeSet<AbstractLocation>>,
    /// `true` when the sets are the analysis' fixpoint. `false` only when
    /// the solve stopped at [`FACT_BUDGET`]: the sets are then an
    /// under-approximation, and consumers should say so.
    pub converged: bool,
}

impl Default for PointsToTable {
    fn default() -> Self {
        Self {
            vars: BTreeMap::new(),
            fields: BTreeMap::new(),
            converged: true,
        }
    }
}

impl PointsToTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Points-to set for a named variable. Returns `None` if the variable
    /// was never assigned in the analysed function.
    pub fn pts_of(&self, v: &Var) -> Option<&BTreeSet<AbstractLocation>> {
        self.vars.get(v)
    }

    /// Points-to set for an abstract `Field(...)` cell.
    pub fn field_pts(&self, loc: &AbstractLocation) -> Option<&BTreeSet<AbstractLocation>> {
        self.fields.get(loc)
    }

    /// `true` iff `a` and `b` could refer to overlapping memory (i.e. the
    /// intersection of their pts-sets is non-empty).
    pub fn may_alias(&self, a: &Var, b: &Var) -> bool {
        match (self.vars.get(a), self.vars.get(b)) {
            (Some(pa), Some(pb)) => pa.intersection(pb).next().is_some(),
            _ => false,
        }
    }
}

/// Owner of the root locations produced by [`analyze`]: an id derived from
/// the function's name alone, since a lone [`IrFunction`] has no graph node.
/// Same-named functions analysed separately share it — compare functions
/// through [`analyze_interprocedural`], which owns locations by [`NodeId`].
pub fn standalone_owner(ir: &IrFunction) -> NodeId {
    NodeId::new("", &ir.name, NodeKind::Function)
}

/// Run Andersen-style field-sensitive points-to analysis on a single
/// function. Returns the saturated [`PointsToTable`], whose root locations
/// are owned by [`standalone_owner`].
pub fn analyze(ir: &IrFunction) -> PointsToTable {
    analyze_with_budget(standalone_owner(ir), ir, FACT_BUDGET)
}

fn analyze_with_budget(owner: NodeId, ir: &IrFunction, budget: usize) -> PointsToTable {
    let mut solver = FunctionSolver::new(owner, ir, budget);
    solver.solve();
    solver.into_table()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;

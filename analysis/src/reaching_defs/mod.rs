//! Flow-sensitive reaching definitions over one [`IrFunction`], on the
//! basic blocks its labels and jumps form.
//!
//! A *definition* writes a [`Place`] — a variable or a field path below it
//! (`req.param.x`, `map["k"]`, `a[]`), at most [`MAX_PLACE_DEPTH`] deep. A
//! *strong* definition overwrites its place and kills every earlier
//! definition of it and of the paths below it (`bar = a; bar = b` — only
//! `b` reaches); a *weak* one (an unknown element `a[i] = v`, a write
//! through a truncated path, a call that may mutate its receiver) adds to
//! them. A read of a place is reached by the definitions of any
//! overlapping place: the place itself, its prefixes (the whole object was
//! replaced) and the paths below it (a field of it was).
//!
//! Branches whose condition folds to a constant ([`consts`]) take only
//! their feasible edge, and blocks unreachable from the entry carry no
//! definitions, so dead code neither defines nor uses anything.
//!
//! Callers can add definitions the IR does not spell ([`ExtraDef`]): an
//! out-parameter written by a call (`fgets(buf, …)`), a value a rule says
//! a variable holds from some point on.

mod blocks;
pub mod consts;
#[cfg(test)]
mod tests;

use std::collections::HashMap;

pub use blocks::Blocks;

use crate::ir::{IrFunction, IrOp, MAX_PLACE_DEPTH, Operand, Place, Var};

/// Index into [`ReachingDefs::defs`].
pub type DefId = usize;

/// Where a definition comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefOrigin {
    /// A parameter or the receiver, at function entry.
    Entry,
    /// The op at this index of `body`.
    Op(usize),
    /// `extras[i]` of [`ReachingDefs::compute`].
    Extra(usize),
}

/// One definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Def {
    pub place: Place,
    pub strong: bool,
    pub origin: DefOrigin,
}

/// A definition the caller adds: `place` is written right after op
/// `after` (or at entry, when `None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtraDef {
    pub after: Option<usize>,
    pub place: Place,
    pub strong: bool,
}

/// Solver options.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// Take only the feasible edge of a branch on a constant condition.
    pub prune_constant_branches: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            prune_constant_branches: true,
        }
    }
}

/// Reaching definitions of one function.
pub struct ReachingDefs {
    pub defs: Vec<Def>,
    pub blocks: Blocks,
    /// Definitions each op makes, then the extras placed after it.
    op_defs: Vec<Vec<DefId>>,
    entry_defs: Vec<DefId>,
    /// Temporaries that name a place (`__t3 = a.b` names `a.b`).
    temp_places: HashMap<Var, Place>,
    /// Per base variable: its definitions (for overlap queries).
    by_base: HashMap<Var, Vec<DefId>>,
    /// Per strong definition: index into `kill_sets` of the definitions
    /// its place covers (one set per distinct place, built once).
    kill_of: Vec<Option<usize>>,
    kill_sets: Vec<BitSet>,
    ins: Vec<BitSet>,
}

impl ReachingDefs {
    pub fn compute(func: &IrFunction, extras: &[ExtraDef], options: Options) -> Self {
        let temp_places = temp_places(func);
        let mut defs: Vec<Def> = Vec::new();
        let mut op_defs: Vec<Vec<DefId>> = vec![Vec::new(); func.body.len()];
        let mut entry_defs = Vec::new();

        for var in func.receiver.iter().chain(&func.params) {
            entry_defs.push(defs.len());
            defs.push(Def {
                place: Place::var(var.clone()),
                strong: true,
                origin: DefOrigin::Entry,
            });
        }
        for (index, op) in func.body.iter().enumerate() {
            for (place, strong) in op_writes(func, index, op, &temp_places) {
                op_defs[index].push(defs.len());
                defs.push(Def {
                    place,
                    strong,
                    origin: DefOrigin::Op(index),
                });
            }
        }
        for (index, extra) in extras.iter().enumerate() {
            let id = defs.len();
            defs.push(Def {
                place: extra.place.clone(),
                strong: extra.strong,
                origin: DefOrigin::Extra(index),
            });
            match extra.after {
                Some(op) if op < op_defs.len() => op_defs[op].push(id),
                _ => entry_defs.push(id),
            }
        }
        let mut by_base: HashMap<Var, Vec<DefId>> = HashMap::new();
        for (id, def) in defs.iter().enumerate() {
            by_base.entry(def.place.base.clone()).or_default().push(id);
        }
        // A strong write kills every definition its place covers: one bit
        // set per distinct written place, so a kill costs a word-wise
        // difference rather than a scan of the variable's definitions.
        let mut kill_index: HashMap<&Place, usize> = HashMap::new();
        let mut kill_sets: Vec<BitSet> = Vec::new();
        let mut kill_of: Vec<Option<usize>> = vec![None; defs.len()];
        for (id, def) in defs.iter().enumerate() {
            if !def.strong {
                continue;
            }
            let index = *kill_index.entry(&def.place).or_insert_with(|| {
                let mut set = BitSet::new(defs.len());
                for &other in &by_base[&def.place.base] {
                    if def.place.covers(&defs[other].place) {
                        set.insert(other);
                    }
                }
                kill_sets.push(set);
                kill_sets.len() - 1
            });
            kill_of[id] = Some(index);
        }

        let constants = if options.prune_constant_branches {
            consts::branch_outcomes(func)
        } else {
            HashMap::new()
        };
        let blocks = Blocks::build(func, &constants);
        let mut solver = Self {
            ins: vec![BitSet::new(defs.len()); blocks.len()],
            defs,
            blocks,
            op_defs,
            entry_defs,
            temp_places,
            by_base,
            kill_of,
            kill_sets,
        };
        solver.solve();
        solver
    }

    fn solve(&mut self) {
        let count = self.blocks.len();
        if count == 0 {
            return;
        }
        let mut outs: Vec<BitSet> = vec![BitSet::new(self.defs.len()); count];
        let mut entry = BitSet::new(self.defs.len());
        self.apply_entry(&mut entry);
        let mut queued = vec![false; count];
        let mut worklist: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
        for block in 0..count {
            if self.blocks.reachable(block) {
                worklist.push_back(block);
                queued[block] = true;
            }
        }
        while let Some(block) = worklist.pop_front() {
            queued[block] = false;
            let mut state = if block == 0 {
                entry.clone()
            } else {
                BitSet::new(self.defs.len())
            };
            for &pred in self.blocks.preds(block) {
                if self.blocks.reachable(pred) {
                    state.union_with(&outs[pred]);
                }
            }
            self.ins[block] = state.clone();
            for op in self.blocks.range(block) {
                self.apply_op(&mut state, op);
            }
            if state != outs[block] {
                outs[block] = state;
                for &succ in self.blocks.succs(block) {
                    if !queued[succ] && self.blocks.reachable(succ) {
                        queued[succ] = true;
                        worklist.push_back(succ);
                    }
                }
            }
        }
    }

    fn apply_entry(&self, state: &mut BitSet) {
        for &id in &self.entry_defs {
            self.gen_def(state, id);
        }
    }

    fn apply_op(&self, state: &mut BitSet, op: usize) {
        for &id in &self.op_defs[op] {
            self.gen_def(state, id);
        }
    }

    fn gen_def(&self, state: &mut BitSet, id: DefId) {
        if let Some(kills) = self.kill_of[id] {
            state.difference_with(&self.kill_sets[kills]);
        }
        state.insert(id);
    }

    /// Whether the point before op `at` can execute.
    pub fn is_reachable(&self, at: usize) -> bool {
        self.blocks
            .block_of(at)
            .is_some_and(|block| self.blocks.reachable(block))
    }

    /// Every definition live at the point before op `at` (after op
    /// `at - 1` and the extras placed after it).
    pub fn live_before(&self, at: usize) -> BitSet {
        let Some(block) = self.blocks.block_of(at) else {
            return BitSet::new(self.defs.len());
        };
        if !self.blocks.reachable(block) {
            return BitSet::new(self.defs.len());
        }
        let mut state = self.ins[block].clone();
        for op in self.blocks.range(block).start..at {
            self.apply_op(&mut state, op);
        }
        state
    }

    /// Definitions of storage overlapping `place` that reach the point
    /// before op `at`.
    pub fn reaching(&self, at: usize, place: &Place) -> Vec<DefId> {
        let live = self.live_before(at);
        self.overlapping(&live, place)
    }

    /// The definitions in `live` of storage overlapping `place`.
    pub fn overlapping(&self, live: &BitSet, place: &Place) -> Vec<DefId> {
        let Some(same_base) = self.by_base.get(&place.base) else {
            return Vec::new();
        };
        same_base
            .iter()
            .copied()
            .filter(|&id| live.contains(id) && self.defs[id].place.overlaps(place))
            .collect()
    }

    /// The place an operand reads: its variable, or for a temporary that
    /// holds a field read, that field's path. `None` for constants.
    pub fn place_of(&self, operand: &Operand) -> Option<Place> {
        match operand {
            Operand::Var(var) => Some(
                self.temp_places
                    .get(var)
                    .cloned()
                    .unwrap_or_else(|| Place::var(var.clone())),
            ),
            Operand::Temp(n) => Some(Place::var(Var::new(format!("__temp{n}")))),
            Operand::Const(_) => None,
        }
    }

    /// Definitions the op at `op` makes (its own, then extras after it).
    pub fn defs_at(&self, op: usize) -> &[DefId] {
        &self.op_defs[op]
    }

    /// Definitions made at entry (parameters, receiver, entry extras).
    pub fn entry_defs(&self) -> &[DefId] {
        &self.entry_defs
    }

    /// Def-use: for each place the op at `op` reads, the definitions
    /// reaching it.
    pub fn uses(&self, func: &IrFunction, op: usize) -> Vec<(Place, Vec<DefId>)> {
        let live = self.live_before(op);
        op_reads(&func.body[op])
            .into_iter()
            .filter_map(|operand| self.place_of(operand))
            .map(|place| {
                let defs = self.overlapping(&live, &place);
                (place, defs)
            })
            .collect()
    }
}

/// Whether `var` is a lowering temporary.
pub fn is_temp(var: &Var) -> bool {
    var.as_str().starts_with("__t")
}

/// Temporaries defined by a field read of a place.
fn temp_places(func: &IrFunction) -> HashMap<Var, Place> {
    let mut places: HashMap<Var, Place> = HashMap::new();
    for op in &func.body {
        if let IrOp::FieldRead { dst, base, field } = op {
            if !is_temp(dst) {
                continue;
            }
            // A temporary names storage only when it holds a field read
            // (`f().x` reads no place).
            let base_place = match base {
                Operand::Var(var) if is_temp(var) => places.get(var).cloned(),
                Operand::Var(var) => Some(Place::var(var.clone())),
                _ => None,
            };
            if let Some(place) = base_place {
                places.insert(dst.clone(), place.field(field).0);
            }
        }
    }
    places
}

/// The places op `index` writes, and whether each write is strong.
fn op_writes(
    func: &IrFunction,
    index: usize,
    op: &IrOp,
    temps: &HashMap<Var, Place>,
) -> Vec<(Place, bool)> {
    let place_of = |operand: &Operand| match operand {
        Operand::Var(var) => temps
            .get(var)
            .cloned()
            .or_else(|| (!is_temp(var)).then(|| Place::var(var.clone()))),
        _ => None,
    };
    match op {
        IrOp::Assign { dst, .. } | IrOp::BinOp { dst, .. } | IrOp::FieldRead { dst, .. } => {
            vec![(Place::var(dst.clone()), true)]
        }
        IrOp::Call { dst, receiver, .. } => {
            let mut writes = Vec::new();
            if let Some(dst) = dst {
                writes.push((Place::var(dst.clone()), true));
            }
            // A method call may mutate its receiver.
            let receiver_place = func
                .call_places(index)
                .and_then(|places| places.receiver.clone())
                .or_else(|| receiver.as_ref().and_then(place_of));
            if let Some(place) = receiver_place {
                writes.push((place, false));
            }
            writes
        }
        IrOp::FieldWrite { base, field, .. } => match place_of(base) {
            Some(base) => {
                let (place, exact) = base.field(field);
                let strong = exact && field != "[]" && place.fields.len() <= MAX_PLACE_DEPTH;
                vec![(place, strong)]
            }
            None => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// The operands an op reads.
pub fn op_reads(op: &IrOp) -> Vec<&Operand> {
    match op {
        IrOp::Assign { src, .. } => vec![src],
        IrOp::BinOp { lhs, rhs, .. } => vec![lhs, rhs],
        IrOp::Call { receiver, args, .. } => receiver.iter().chain(args).collect(),
        IrOp::FieldRead { base, .. } => vec![base],
        IrOp::FieldWrite { base, src, .. } => vec![base, src],
        IrOp::Branch { cond, .. } => vec![cond],
        IrOp::Return { value } => value.iter().collect(),
        IrOp::Jump { .. } | IrOp::Label(_) | IrOp::Nop => Vec::new(),
    }
}

/// A fixed-size bit set over definition ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BitSet {
    words: Vec<u64>,
}

impl BitSet {
    pub fn new(len: usize) -> Self {
        Self {
            words: vec![0; len.div_ceil(64)],
        }
    }

    pub fn insert(&mut self, index: usize) {
        self.words[index / 64] |= 1 << (index % 64);
    }

    pub fn remove(&mut self, index: usize) {
        self.words[index / 64] &= !(1 << (index % 64));
    }

    pub fn contains(&self, index: usize) -> bool {
        self.words
            .get(index / 64)
            .is_some_and(|word| word & (1 << (index % 64)) != 0)
    }

    pub fn difference_with(&mut self, other: &BitSet) {
        for (word, other) in self.words.iter_mut().zip(&other.words) {
            *word &= !other;
        }
    }

    pub fn union_with(&mut self, other: &BitSet) {
        for (word, other) in self.words.iter_mut().zip(&other.words) {
            *word |= other;
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.words.iter().enumerate().flat_map(|(i, &word)| {
            (0..64)
                .filter(move |bit| word & (1 << bit) != 0)
                .map(move |bit| i * 64 + bit)
        })
    }
}

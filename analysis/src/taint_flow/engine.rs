//! One function's taint: marks onto reaching definitions, library models
//! and callee summaries onto calls, then a backward search from every
//! output (a sink, the return value, storage callers see) that records the
//! roots it reaches — sources (local, or handed back by a callee) for
//! findings, inputs (parameters, receiver, shared storage) for the
//! function's own summary.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use super::guards::{self, Dominators, Guard};
use super::summary::{
    self,
    Access,
    FuncId,
    MarkRef,
    Path,
    SinkFact,
    Slot,
    SourceFact,
    Step,
    Summary,
};
use super::{Budget, CallResolution, InterFlow, Mark, TaintSpec};
use crate::ir::shared::{shared_key, shared_var};
use crate::ir::{ExprValue, IrFunction, IrOp, Operand, Place, Var};
use crate::propagation_rules::PropagationRules;
use crate::reaching_defs::{BitSet, DefId, DefOrigin, ExtraDef, Options, ReachingDefs, is_temp};

/// What a call's result carries.
#[derive(Debug, Clone)]
enum ResultRule {
    /// Its receiver's and arguments' data.
    Default,
    /// Nothing.
    Clean,
    /// Nothing known (project code without a summary).
    Opaque,
    /// The receiver's element under a constant key (or the whole receiver).
    KeyedRead(Option<String>),
    /// Exactly these reads (a positional list element).
    Reads(Vec<(Read, usize)>),
    /// What the callee's summary says.
    Summary(Arc<Summary>),
}

/// The model of one call.
#[derive(Debug, Clone)]
struct CallModel {
    result: ResultRule,
    /// The receiver takes its arguments' data.
    receiver_from_args: bool,
}

/// A read a definition's value depends on.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum Read {
    Operand(Operand),
    Place(Place),
}

/// A read, where it happens, and the callee path it crosses on the way
/// (from the callee's input to its output).
type Input = (Read, usize, Option<Path>);

/// Where a marked value is defined.
#[derive(Debug, Clone)]
enum Target {
    /// The value this op computes.
    Op(usize),
    /// This storage, written after the op (or at entry).
    Storage(Option<usize>, Place),
}

/// What an extra definition is.
#[derive(Debug, Clone)]
enum ExtraKind {
    /// A source (index into `spec.sources`).
    Source(usize),
    /// Sanitized: never tainted.
    Clean,
    /// Data from these reads, at this point.
    From(Vec<(Read, usize)>),
    /// Shared storage as the caller left it.
    Shared(String),
    /// Storage a project call writes (from its summary).
    CallWrite {
        inputs: Vec<Input>,
        sources: Vec<SourceFact>,
    },
}

/// What a search reached that data can come from.
#[derive(Debug, Clone)]
pub(super) enum Root {
    /// A source mark of this function.
    Local(usize),
    /// A source a callee hands back (at the call the root definition is).
    External(SourceFact),
    /// A value from the caller.
    Input(Access),
}

/// A callee's sink an argument of a call reaches.
#[derive(Debug, Clone)]
struct CallSink {
    op: usize,
    read: Read,
    fact: SinkFact,
}

/// How the search reached a definition: the definition it feeds (toward
/// the output), the place read, and the callee path crossed.
#[derive(Debug, Clone)]
struct Link {
    toward: Option<DefId>,
    read: Option<Place>,
    via: Option<Path>,
}

/// What one backward search found.
pub(super) struct Found {
    links: HashMap<DefId, Link>,
    roots: Vec<(Root, DefId)>,
}

pub(super) struct Engine<'f> {
    func: &'f IrFunction,
    id: FuncId,
    models: HashMap<usize, CallModel>,
    /// The op defining each temporary.
    temp_defs: HashMap<&'f Var, usize>,
    extras: Vec<ExtraDef>,
    extra_kinds: Vec<ExtraKind>,
    /// Ops whose result is a source (op → source index).
    source_ops: HashMap<usize, usize>,
    /// Ops whose result is sanitized.
    clean_ops: HashSet<usize>,
    /// Extra reads flowing into an op's result (rule propagators).
    op_inputs: HashMap<usize, Vec<(Read, usize)>>,
    /// Sinks: (spec index, operand, point, where).
    sinks: Vec<(usize, Operand, usize, crate::ir::Span)>,
    call_sinks: Vec<CallSink>,
    /// Guards: (check value, checked place, safe when the check holds).
    guard_marks: Vec<(Operand, Place, bool)>,
    guards: Vec<Guard>,
    dominators: Option<Dominators>,
    rd: Option<ReachingDefs>,
    live: HashMap<usize, BitSet>,
    /// Recorded values by their byte range.
    by_range: HashMap<(usize, usize), Vec<usize>>,
    /// `Call` ops by the operands they take as arguments.
    calls_taking: HashMap<&'f Operand, Vec<usize>>,
}

/// The last name of a callee or qualified name (`a.b.c` → `c`,
/// `x::Y::z` → `z`, `new Foo` → `Foo`).
pub(super) fn last_name(text: &str) -> &str {
    text.rsplit(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
        .find(|part| !part.is_empty())
        .unwrap_or(text)
}

/// A constant key as an element field (`"k"` → `[k]`), as the lowering
/// spells element accesses.
fn key_field(operand: Option<&Operand>) -> Option<String> {
    match operand {
        Some(Operand::Const(text)) => Some(format!("[{}]", text.trim_matches(['"', '\'']))),
        _ => None,
    }
}

/// `place` with `fields` below it.
fn below(place: &Place, fields: &[String]) -> Place {
    let mut out = place.clone();
    for field in fields {
        out = out.field(field).0;
    }
    out
}

impl<'f> Engine<'f> {
    pub fn new(
        func: &'f IrFunction,
        id: FuncId,
        rules: &PropagationRules,
        resolve: &dyn Fn(usize) -> CallResolution,
    ) -> Self {
        let mut engine = Self {
            by_range: HashMap::new(),
            calls_taking: HashMap::new(),
            func,
            id,
            models: HashMap::new(),
            temp_defs: HashMap::new(),
            extras: Vec::new(),
            extra_kinds: Vec::new(),
            source_ops: HashMap::new(),
            clean_ops: HashSet::new(),
            op_inputs: HashMap::new(),
            sinks: Vec::new(),
            call_sinks: Vec::new(),
            guard_marks: Vec::new(),
            guards: Vec::new(),
            dominators: None,
            rd: None,
            live: HashMap::new(),
        };
        engine.model_calls(rules, resolve);
        engine.share_entries();
        for (index, value) in func.values.iter().enumerate() {
            engine
                .by_range
                .entry((value.span.start_byte, value.span.end_byte))
                .or_default()
                .push(index);
        }
        for (index, op) in func.body.iter().enumerate() {
            if let IrOp::Call { args, .. } = op {
                for arg in args {
                    engine.calls_taking.entry(arg).or_default().push(index);
                }
            }
        }
        engine
    }

    /// Library models, positional lists and callee summaries onto calls.
    fn model_calls(&mut self, rules: &PropagationRules, resolve: &dyn Fn(usize) -> CallResolution) {
        let func = self.func;
        let positional = super::lists::positional_reads(func, rules);
        let mut fluent_roots: HashMap<usize, Place> = HashMap::new();
        for (index, op) in func.body.iter().enumerate() {
            match op {
                IrOp::Assign { dst, .. }
                | IrOp::BinOp { dst, .. }
                | IrOp::FieldRead { dst, .. }
                    if is_temp(dst) =>
                {
                    self.temp_defs.insert(dst, index);
                }
                IrOp::Call {
                    dst,
                    callee,
                    receiver,
                    args,
                } => {
                    if let Some(dst) = dst.as_ref().filter(|dst| is_temp(dst)) {
                        self.temp_defs.insert(dst, index);
                    }
                    let resolution = resolve(index);
                    if resolution.in_project {
                        let result = match resolution.summary {
                            Some(summary) => {
                                self.apply_summary(index, &summary);
                                ResultRule::Summary(summary)
                            }
                            None => ResultRule::Opaque,
                        };
                        self.models.insert(
                            index,
                            CallModel {
                                result,
                                receiver_from_args: false,
                            },
                        );
                        continue;
                    }
                    let mut names = resolution.names;
                    names.push(callee.clone());
                    let names: Vec<&str> = names.iter().map(|name| last_name(name)).collect();
                    let find = |test: &dyn Fn(&str) -> bool| names.iter().any(|name| test(name));
                    let mut model = CallModel {
                        result: ResultRule::Default,
                        receiver_from_args: find(&|name| rules.is_receiver_write(name)),
                    };
                    if let Some((arg, append)) = positional.get(&index) {
                        model.result =
                            ResultRule::Reads(vec![(Read::Operand(arg.clone()), *append)]);
                    } else if find(&|name| rules.is_clean_result(name)) {
                        model.result = ResultRule::Clean;
                    } else if let Some(key) = names.iter().find_map(|name| rules.keyed_read(name)) {
                        if receiver.is_some() {
                            model.result = ResultRule::KeyedRead(key_field(args.get(key)));
                        }
                    }
                    let places = func.call_places(index);
                    let receiver_place = places.and_then(|p| p.receiver.clone());
                    // A builder chain (`sb.append(a).append(b)`): each call
                    // returns its receiver, so the chain updates the root.
                    let chained_root = receiver
                        .as_ref()
                        .and_then(|r| match r {
                            Operand::Var(var) => self.temp_defs.get(var),
                            _ => None,
                        })
                        .and_then(|op| fluent_roots.get(op))
                        .cloned();
                    if model.receiver_from_args {
                        if let Some(root) = receiver_place.clone().or(chained_root) {
                            fluent_roots.insert(index, root.clone());
                            if receiver_place.is_none() {
                                self.extras.push(ExtraDef {
                                    after: Some(index),
                                    place: root,
                                    strong: false,
                                });
                                self.extra_kinds.push(ExtraKind::From(
                                    args.iter()
                                        .map(|arg| (Read::Operand(arg.clone()), index))
                                        .collect(),
                                ));
                            }
                        }
                    }
                    // `map.put("k", v)`: `map["k"]` (or all of `map`) takes `v`.
                    if let Some((key, value)) =
                        names.iter().find_map(|name| rules.keyed_write(name))
                    {
                        if let (Some(place), Some(value)) = (&receiver_place, args.get(value)) {
                            let (place, strong) = match key_field(args.get(key)) {
                                Some(field) => place.field(&field),
                                None => (place.clone(), false),
                            };
                            self.extras.push(ExtraDef {
                                after: Some(index),
                                place,
                                strong,
                            });
                            self.extra_kinds
                                .push(ExtraKind::From(vec![(Read::Operand(value.clone()), index)]));
                            model.receiver_from_args = false;
                        }
                    }
                    // `strcpy(dst, src)`: `dst`'s storage takes the sources.
                    if let Some((dst, src)) =
                        names.iter().find_map(|name| rules.argument_write(name))
                    {
                        let dst_place = places.and_then(|p| p.args.get(dst).cloned().flatten());
                        if let Some(place) = dst_place {
                            let reads: Vec<(Read, usize)> = args
                                .iter()
                                .enumerate()
                                .filter(|(i, _)| *i >= src && *i != dst)
                                .map(|(_, arg)| (Read::Operand(arg.clone()), index))
                                .collect();
                            self.extras.push(ExtraDef {
                                after: Some(index),
                                place,
                                strong: false,
                            });
                            self.extra_kinds.push(ExtraKind::From(reads));
                        }
                    }
                    self.models.insert(index, model);
                }
                _ => {}
            }
        }
    }

    /// The caller-side read of a callee input at call op `op`.
    fn map_access(&self, op: usize, access: &Access) -> Option<Read> {
        let IrOp::Call { receiver, args, .. } = &self.func.body[op] else {
            return None;
        };
        let places = self.func.call_places(op);
        let (place, operand) = match &access.slot {
            Slot::Param(index) => (
                places.and_then(|p| p.args.get(*index).cloned().flatten()),
                args.get(*index).cloned(),
            ),
            Slot::Receiver => (places.and_then(|p| p.receiver.clone()), receiver.clone()),
            Slot::Global(key) => (Some(Place::var(shared_var(key))), None),
        };
        match (place, operand) {
            (Some(place), _) => Some(Read::Place(below(&place, &access.fields))),
            (None, Some(operand)) => Some(Read::Operand(operand)),
            (None, None) => None,
        }
    }

    /// The caller-side storage a callee writes through `access`.
    fn map_target(&self, op: usize, access: &Access) -> Option<Place> {
        let places = self.func.call_places(op);
        let place = match &access.slot {
            Slot::Param(index) => places.and_then(|p| p.args.get(*index).cloned().flatten()),
            Slot::Receiver => places.and_then(|p| p.receiver.clone()),
            Slot::Global(key) => Some(Place::var(shared_var(key))),
        }?;
        Some(below(&place, &access.fields))
    }

    /// A callee summary's writes and sinks at call op `op`.
    fn apply_summary(&mut self, op: usize, summary: &Summary) {
        for (target, output) in &summary.writes {
            let Some(place) = self.map_target(op, target) else {
                continue;
            };
            let inputs: Vec<Input> = output
                .inputs
                .iter()
                .filter_map(|(access, path)| {
                    self.map_access(op, access)
                        .map(|read| (read, op, Some(path.clone())))
                })
                .collect();
            self.extras.push(ExtraDef {
                after: Some(op),
                place,
                strong: false,
            });
            self.extra_kinds.push(ExtraKind::CallWrite {
                inputs,
                sources: output.sources.clone(),
            });
        }
        for (input, fact) in &summary.sinks {
            if let Some(read) = self.map_access(op, input) {
                self.call_sinks.push(CallSink {
                    op,
                    read,
                    fact: fact.clone(),
                });
            }
        }
    }

    /// Shared storage holds, at entry, what the caller left in it: one
    /// entry definition per shared variable the function or its callees'
    /// summaries use.
    fn share_entries(&mut self) {
        let mut keys: Vec<String> = Vec::new();
        let mut add = |key: &str| {
            if !keys.iter().any(|k| k == key) {
                keys.push(key.to_string());
            }
        };
        for op in &self.func.body {
            let vars = crate::reaching_defs::op_reads(op)
                .into_iter()
                .filter_map(|operand| match operand {
                    Operand::Var(var) => Some(var),
                    _ => None,
                })
                .chain(match op {
                    IrOp::Assign { dst, .. } => Some(dst),
                    _ => None,
                });
            for var in vars {
                if let Some(key) = shared_key(var) {
                    add(key);
                }
            }
        }
        for model in self.models.values() {
            if let ResultRule::Summary(summary) = &model.result {
                let inputs = summary
                    .returns
                    .inputs
                    .iter()
                    .map(|(a, _)| a)
                    .chain(summary.writes.iter().map(|(t, _)| t))
                    .chain(
                        summary
                            .writes
                            .iter()
                            .flat_map(|(_, o)| o.inputs.iter().map(|(a, _)| a)),
                    )
                    .chain(summary.sinks.iter().map(|(a, _)| a));
                for access in inputs {
                    if let Slot::Global(key) = &access.slot {
                        add(key);
                    }
                }
            }
        }
        for key in keys {
            self.extras.push(ExtraDef {
                after: None,
                place: Place::var(shared_var(&key)),
                strong: false,
            });
            self.extra_kinds.push(ExtraKind::Shared(key));
        }
    }

    /// The recorded value of the node `mark` names: the value with its
    /// exact range (of its kind, when several), else the smallest one
    /// around it.
    fn locate(&self, mark: &Mark) -> Option<&'f ExprValue> {
        let values = &self.func.values;
        let exact: Vec<&ExprValue> = self
            .by_range
            .get(&(mark.start_byte, mark.end_byte))
            .map(|indices| indices.iter().map(|&i| &values[i]).collect())
            .unwrap_or_default();
        if let Some(kind) = mark.kind {
            if let Some(value) = exact.iter().rev().find(|v| v.kind == kind) {
                return Some(value);
            }
        }
        if let Some(value) = exact.last() {
            return Some(value);
        }
        values
            .iter()
            .filter(|v| v.span.start_byte <= mark.start_byte && mark.end_byte <= v.span.end_byte)
            .min_by_key(|v| v.span.end_byte - v.span.start_byte)
    }

    /// The op computing a temporary's value.
    fn defining_op(&self, operand: &Operand) -> Option<usize> {
        match operand {
            Operand::Var(var) if is_temp(var) => self.temp_defs.get(var).copied(),
            _ => None,
        }
    }

    /// The call `value` is an argument of: the smallest value around it
    /// that a `Call` op computes with `value` among its arguments.
    fn enclosing_call(&self, value: &ExprValue) -> Option<usize> {
        self.calls_taking
            .get(&value.operand)?
            .iter()
            .map(|&op| (self.func.span(op), op))
            .filter(|(span, _)| span.line > 0 && span.contains(&value.span))
            .max_by_key(|(span, _)| span.start_byte)
            .map(|(_, op)| op)
    }

    /// How a marked value becomes a definition: the op computing it (a
    /// call's result), the storage a call writes through it (an
    /// out-argument), a parameter at entry, or the op computing it
    /// otherwise; else the storage it names from that point on.
    fn target(&self, mark: &Mark) -> Option<Target> {
        if let Some(index) =
            self.func.param_spans.iter().position(|span| {
                span.start_byte <= mark.start_byte && mark.end_byte <= span.end_byte
            })
        {
            let param = self.func.params.get(index)?;
            return Some(Target::Storage(None, Place::var(param.clone())));
        }
        let value = self.locate(mark)?;
        let op = self.defining_op(&value.operand);
        if let Some(op) = op.filter(|&op| matches!(self.func.body[op], IrOp::Call { .. })) {
            return Some(Target::Op(op));
        }
        if let (Some(place), Some(call)) = (&value.place, self.enclosing_call(value)) {
            return Some(Target::Storage(Some(call), place.clone()));
        }
        if let Some(op) = op {
            return Some(Target::Op(op));
        }
        let place = value.place.clone()?;
        Some(Target::Storage(value.at.checked_sub(1), place))
    }

    /// Where a source's value is defined. A source that reads storage
    /// (`request.args`, `$_GET['q']`, `req.query`) makes that storage
    /// untrusted from entry, so every read of it or below it is tainted.
    fn source_target(&self, mark: &Mark) -> Option<Target> {
        let target = self.target(mark)?;
        if let Target::Op(op) = target {
            if matches!(self.func.body[op], IrOp::FieldRead { .. }) {
                if let Some(place) = self.locate(mark).and_then(|value| value.place.clone()) {
                    return Some(Target::Storage(None, place));
                }
            }
        }
        Some(target)
    }

    pub fn mark(&mut self, spec: &TaintSpec) {
        for (index, mark) in spec.sources.iter().enumerate() {
            match self.source_target(mark) {
                Some(Target::Op(op)) => {
                    self.source_ops.insert(op, index);
                }
                Some(Target::Storage(after, place)) => {
                    self.extras.push(ExtraDef {
                        after,
                        place,
                        strong: false,
                    });
                    self.extra_kinds.push(ExtraKind::Source(index));
                }
                None => {}
            }
        }
        for mark in &spec.sanitizers {
            match self.target(mark) {
                Some(Target::Op(op)) => {
                    self.clean_ops.insert(op);
                }
                // Sanitized in place (`sanitize(buf)`): overwrites it.
                Some(Target::Storage(after @ Some(_), place)) => {
                    self.extras.push(ExtraDef {
                        after,
                        place,
                        strong: true,
                    });
                    self.extra_kinds.push(ExtraKind::Clean);
                }
                _ => {}
            }
        }
        for propagator in &spec.propagators {
            let Some(from) = self.locate(&propagator.from) else {
                continue;
            };
            let read = (Read::Operand(from.operand.clone()), from.at);
            match self.target(&propagator.to) {
                Some(Target::Op(op)) => self.op_inputs.entry(op).or_default().push(read),
                Some(Target::Storage(after, place)) => {
                    self.extras.push(ExtraDef {
                        after,
                        place,
                        strong: false,
                    });
                    self.extra_kinds.push(ExtraKind::From(vec![read]));
                }
                None => {}
            }
        }
        for guard in &spec.guards {
            let (Some(check), Some(value)) = (self.locate(&guard.check), self.locate(&guard.value))
            else {
                continue;
            };
            let place = value.place.clone().or_else(|| match &value.operand {
                Operand::Var(var) if !is_temp(var) => Some(Place::var(var.clone())),
                _ => None,
            });
            if let Some(place) = place {
                self.guard_marks
                    .push((check.operand.clone(), place, guard.safe_when_true));
            }
        }
        for (index, mark) in spec.sinks.iter().enumerate() {
            if let Some(value) = self.locate(mark) {
                self.sinks
                    .push((index, value.operand.clone(), value.at, value.span));
            }
        }
    }

    fn rd(&self) -> &ReachingDefs {
        self.rd.as_ref().expect("solved")
    }

    /// Solve reaching definitions (and the guards on them).
    pub fn solve(&mut self) {
        self.rd = Some(ReachingDefs::compute(
            self.func,
            &self.extras,
            Options::default(),
        ));
        if !self.guard_marks.is_empty() {
            let rd = self.rd();
            let mut resolved = Vec::new();
            for (check, place, safe) in &self.guard_marks {
                resolved.extend(guards::resolve(self.func, rd, check, place, *safe));
            }
            if !resolved.is_empty() {
                self.dominators = Some(Dominators::compute(&rd.blocks));
            }
            self.guards = resolved;
        }
    }

    /// The place a read reads (`None`: a constant).
    fn read_place(read: &Read) -> Option<Place> {
        match read {
            Read::Place(place) => Some(place.clone()),
            Read::Operand(Operand::Var(var)) => Some(Place::var(var.clone())),
            Read::Operand(_) => None,
        }
    }

    /// Definitions reaching a read at the point before op `at` — none when
    /// a guard makes the read safe.
    fn resolve(&mut self, read: &Read, at: usize) -> Vec<DefId> {
        let Some(place) = Self::read_place(read) else {
            return Vec::new();
        };
        if !self.live.contains_key(&at) {
            let live = self.rd().live_before(at);
            self.live.insert(at, live);
        }
        let defs = self.rd().overlapping(&self.live[&at], &place);
        if self.is_guarded(&place, at, &defs) {
            return Vec::new();
        }
        defs
    }

    /// Whether a guard's safe edge dominates `at` and the value read there
    /// is the one it checked.
    fn is_guarded(&self, place: &Place, at: usize, defs: &[DefId]) -> bool {
        let Some(dominators) = &self.dominators else {
            return false;
        };
        let Some(block) = self.rd().blocks.block_of(at) else {
            return false;
        };
        !defs.is_empty()
            && self.guards.iter().any(|guard| {
                place.starts_with(&guard.place)
                    && dominators.dominates(guard.safe, block)
                    && defs.iter().all(|def| guard.checked.contains(def))
            })
    }

    /// What a definition's value is computed from.
    fn inputs(&self, def: DefId) -> Vec<Input> {
        let rd = self.rd();
        let the_def = &rd.defs[def];
        let op = match the_def.origin {
            DefOrigin::Entry => return Vec::new(),
            DefOrigin::Extra(index) => {
                return match &self.extra_kinds[index] {
                    ExtraKind::From(reads) => reads
                        .iter()
                        .map(|(read, at)| (read.clone(), *at, None))
                        .collect(),
                    ExtraKind::CallWrite { inputs, .. } => inputs.clone(),
                    ExtraKind::Source(_) | ExtraKind::Clean | ExtraKind::Shared(_) => Vec::new(),
                };
            }
            DefOrigin::Op(op) => op,
        };
        let operand = |operand: &Operand| (Read::Operand(operand.clone()), op, None);
        let mut reads = match &self.func.body[op] {
            IrOp::Assign { src, .. } => vec![operand(src)],
            IrOp::BinOp {
                lhs, op: kind, rhs, ..
            } => {
                if kind.is_comparison() {
                    Vec::new()
                } else {
                    vec![operand(lhs), operand(rhs)]
                }
            }
            IrOp::FieldRead { dst, base, .. } => {
                // A read of a place reads its definitions; of a call's
                // result, the result.
                match rd.place_of(&Operand::Var(dst.clone())) {
                    Some(place) if place != Place::var(dst.clone()) => {
                        vec![(Read::Place(place), op, None)]
                    }
                    _ => vec![operand(base)],
                }
            }
            IrOp::FieldWrite { src, .. } => vec![operand(src)],
            IrOp::Call {
                dst,
                receiver,
                args,
                ..
            } => {
                let model = self.models.get(&op);
                let is_result = dst
                    .as_ref()
                    .is_some_and(|dst| the_def.place == Place::var(dst.clone()));
                if is_result {
                    match model.map(|m| &m.result) {
                        Some(ResultRule::Clean | ResultRule::Opaque) => Vec::new(),
                        Some(ResultRule::Reads(reads)) => reads
                            .iter()
                            .map(|(read, at)| (read.clone(), *at, None))
                            .collect(),
                        Some(ResultRule::Summary(summary)) => summary
                            .returns
                            .inputs
                            .iter()
                            .filter_map(|(access, path)| {
                                self.map_access(op, access)
                                    .map(|read| (read, op, Some(path.clone())))
                            })
                            .collect(),
                        Some(ResultRule::KeyedRead(key)) => {
                            let receiver_place = self
                                .func
                                .call_places(op)
                                .and_then(|p| p.receiver.clone())
                                .or_else(|| receiver.as_ref().and_then(|r| rd.place_of(r)));
                            match (receiver_place, key) {
                                (Some(place), Some(key)) => {
                                    vec![(Read::Place(place.field(key).0), op, None)]
                                }
                                _ => receiver.iter().map(operand).collect(),
                            }
                        }
                        _ => receiver.iter().chain(args).map(operand).collect(),
                    }
                } else if model.is_some_and(|m| m.receiver_from_args) {
                    // The receiver's update: the arguments' data.
                    args.iter().map(operand).collect()
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        };
        if let Some(extra) = self.op_inputs.get(&op) {
            reads.extend(extra.iter().map(|(read, at)| (read.clone(), *at, None)));
        }
        reads
    }

    /// Whether `def` is the value op `op` computes (not a receiver or
    /// storage it may also update).
    fn is_own_result(&self, def: DefId, op: usize) -> bool {
        let place = &self.rd().defs[def].place;
        match &self.func.body[op] {
            IrOp::Call { dst: Some(dst), .. }
            | IrOp::Assign { dst, .. }
            | IrOp::BinOp { dst, .. }
            | IrOp::FieldRead { dst, .. } => place.fields.is_empty() && place.base == *dst,
            _ => false,
        }
    }

    fn is_clean(&self, def: DefId) -> bool {
        match self.rd().defs[def].origin {
            DefOrigin::Op(op) => self.clean_ops.contains(&op) && self.is_own_result(def, op),
            DefOrigin::Extra(index) => matches!(self.extra_kinds[index], ExtraKind::Clean),
            DefOrigin::Entry => false,
        }
    }

    /// The roots `def` is (a source, a callee's source, a caller's value),
    /// reached by reading `read`.
    fn roots_of(&self, def: DefId, read: Option<&Place>) -> Vec<Root> {
        let rd = self.rd();
        let the_def = &rd.defs[def];
        let fields = |base: &Var| -> Vec<String> {
            read.filter(|place| place.base == *base)
                .map(|place| place.fields.clone())
                .unwrap_or_default()
        };
        match the_def.origin {
            DefOrigin::Op(op) => {
                let mut roots = Vec::new();
                if self.is_own_result(def, op) {
                    if let Some(&source) = self.source_ops.get(&op) {
                        roots.push(Root::Local(source));
                    }
                    if let Some(CallModel {
                        result: ResultRule::Summary(summary),
                        ..
                    }) = self.models.get(&op)
                    {
                        roots.extend(
                            summary
                                .returns
                                .sources
                                .iter()
                                .map(|fact| Root::External(fact.clone())),
                        );
                    }
                }
                roots
            }
            DefOrigin::Extra(index) => match &self.extra_kinds[index] {
                ExtraKind::Source(source) => vec![Root::Local(*source)],
                ExtraKind::Shared(key) => vec![Root::Input(Access {
                    slot: Slot::Global(key.clone()),
                    fields: fields(&the_def.place.base),
                })],
                ExtraKind::CallWrite { sources, .. } => sources
                    .iter()
                    .map(|fact| Root::External(fact.clone()))
                    .collect(),
                _ => Vec::new(),
            },
            DefOrigin::Entry => {
                let base = &the_def.place.base;
                let slot = if self.func.receiver.as_ref() == Some(base) {
                    Some(Slot::Receiver)
                } else {
                    self.func
                        .params
                        .iter()
                        .position(|param| param == base)
                        .map(Slot::Param)
                };
                slot.map(|slot| {
                    Root::Input(Access {
                        slot,
                        fields: fields(base),
                    })
                })
                .into_iter()
                .collect()
            }
        }
    }

    fn hop(&self, def: DefId) -> Step {
        let rd = self.rd();
        let at_op = |op: usize| Step {
            func: self.id,
            span: self.func.span(op),
            op: Some(op),
        };
        match rd.defs[def].origin {
            DefOrigin::Op(op) => at_op(op),
            DefOrigin::Extra(index) => match self.extras[index].after {
                Some(op) => at_op(op),
                None => self.entry_step(&rd.defs[def].place),
            },
            DefOrigin::Entry => self.entry_step(&rd.defs[def].place),
        }
    }

    fn entry_step(&self, place: &Place) -> Step {
        let span = self
            .func
            .params
            .iter()
            .position(|param| *param == place.base)
            .and_then(|index| self.func.param_spans.get(index).copied())
            .unwrap_or_default();
        Step {
            func: self.id,
            span,
            op: None,
        }
    }

    fn op_step(&self, op: usize) -> Step {
        Step {
            func: self.id,
            span: self.func.span(op),
            op: Some(op),
        }
    }

    /// Search backward from `start` (reads at points) for every root.
    fn search(&mut self, start: &[(Read, usize)], budget: &mut Budget) -> Found {
        let mut found = Found {
            links: HashMap::new(),
            roots: Vec::new(),
        };
        let mut queue: VecDeque<DefId> = VecDeque::new();
        for (read, at) in start {
            let place = Self::read_place(read);
            for def in self.resolve(read, *at) {
                if let std::collections::hash_map::Entry::Vacant(entry) = found.links.entry(def) {
                    entry.insert(Link {
                        toward: None,
                        read: place.clone(),
                        via: None,
                    });
                    queue.push_back(def);
                }
            }
        }
        while let Some(def) = queue.pop_front() {
            if !budget.spend(1) {
                break;
            }
            if self.is_clean(def) {
                continue;
            }
            let read = found.links[&def].read.clone();
            for root in self.roots_of(def, read.as_ref()) {
                found.roots.push((root, def));
            }
            for (read, point, via) in self.inputs(def) {
                let place = Self::read_place(&read);
                for next in self.resolve(&read, point) {
                    if let std::collections::hash_map::Entry::Vacant(entry) =
                        found.links.entry(next)
                    {
                        entry.insert(Link {
                            toward: Some(def),
                            read: place.clone(),
                            via: via.clone(),
                        });
                        queue.push_back(next);
                    }
                }
            }
        }
        found
    }

    /// The steps from `root` (reached at `def`) to the search's start.
    fn chain(&self, found: &Found, root: &Root, def: DefId) -> Vec<Step> {
        let mut steps: Vec<Step> = Vec::new();
        if let Root::External(fact) = root {
            steps.extend(fact.path.iter().copied());
        }
        steps.push(self.hop(def));
        let mut current = def;
        // Each definition is linked once, so the chain ends.
        for _ in 0..found.links.len() {
            let link = &found.links[&current];
            let Some(next) = link.toward else {
                break;
            };
            if let Some(via) = &link.via {
                steps.extend(via.iter().copied());
            }
            steps.push(self.hop(next));
            current = next;
        }
        steps
    }

    fn source_ref(&self, root: &Root) -> Option<MarkRef> {
        match root {
            Root::Local(index) => Some(MarkRef {
                func: self.id,
                index: *index,
            }),
            Root::External(fact) => Some(fact.source),
            Root::Input(_) => None,
        }
    }

    /// Run every search: local sinks, call sinks, the return value and
    /// what callers see written. Returns the flows found and the summary.
    pub fn run(&mut self, budget: &mut Budget) -> (Vec<InterFlow>, Summary) {
        let mut flows = Vec::new();
        let mut summary = Summary::default();
        let rd_reachable = |engine: &Self, at: usize| engine.rd().is_reachable(at);

        // Local sinks.
        let sinks = std::mem::take(&mut self.sinks);
        for (index, operand, at, span) in &sinks {
            if !rd_reachable(self, *at) {
                continue;
            }
            let found = self.search(&[(Read::Operand(operand.clone()), *at)], budget);
            let sink = MarkRef {
                func: self.id,
                index: *index,
            };
            let sink_step = Step {
                func: self.id,
                span: *span,
                op: None,
            };
            self.collect_sink(&found, sink, &[sink_step], &mut flows, &mut summary);
        }
        self.sinks = sinks;

        // Callee sinks an argument reaches.
        let call_sinks = std::mem::take(&mut self.call_sinks);
        for call_sink in &call_sinks {
            if !rd_reachable(self, call_sink.op) {
                continue;
            }
            let found = self.search(&[(call_sink.read.clone(), call_sink.op)], budget);
            let mut tail = vec![self.op_step(call_sink.op)];
            tail.extend(call_sink.fact.path.iter().copied());
            self.collect_sink(&found, call_sink.fact.sink, &tail, &mut flows, &mut summary);
        }
        self.call_sinks = call_sinks;

        // The return value.
        let returns: Vec<(usize, Operand)> = self
            .func
            .body
            .iter()
            .enumerate()
            .filter_map(|(index, op)| match op {
                IrOp::Return { value: Some(value) } => Some((index, value.clone())),
                _ => None,
            })
            .filter(|(index, _)| rd_reachable(self, *index))
            .collect();
        if !returns.is_empty() {
            let start: Vec<(Read, usize)> = returns
                .iter()
                .map(|(index, value)| (Read::Operand(value.clone()), *index))
                .collect();
            let found = self.search(&start, budget);
            let end = self.op_step(returns[0].0);
            for (root, def) in &found.roots {
                let mut steps = self.chain(&found, root, *def);
                steps.push(end);
                match root {
                    Root::Input(access) => {
                        summary.add_return_input(access.clone(), summary::path(steps))
                    }
                    _ => {
                        if let Some(source) = self.source_ref(root) {
                            summary.add_return_source(SourceFact {
                                source,
                                path: summary::path(steps),
                            });
                        }
                    }
                }
            }
        }

        // Storage callers see: parameters' objects, the receiver, shared
        // storage, as they are when the function exits.
        let exits: Vec<usize> = self
            .func
            .body
            .iter()
            .enumerate()
            .filter(|(index, op)| {
                (matches!(op, IrOp::Return { .. }) || *index + 1 == self.func.body.len())
                    && rd_reachable(self, *index)
            })
            .map(|(index, _)| index)
            .collect();
        if !exits.is_empty() {
            for (slot, place) in self.outputs() {
                let start: Vec<(Read, usize)> = exits
                    .iter()
                    .map(|&at| (Read::Place(place.clone()), at))
                    .collect();
                if self.rebound_at_exit(&place, &exits) {
                    continue;
                }
                let found = self.search(&start, budget);
                let end = self.op_step(exits[0]);
                let mut output = summary::Output::default();
                for (root, def) in &found.roots {
                    // What the caller passed in stays what it was.
                    if matches!(root, Root::Input(a) if a.slot == slot && a.fields.is_empty()) {
                        continue;
                    }
                    let mut steps = self.chain(&found, root, *def);
                    steps.push(end);
                    match root {
                        Root::Input(access) => {
                            if !output.inputs.iter().any(|(a, _)| a == access) {
                                output.inputs.push((access.clone(), summary::path(steps)));
                            }
                        }
                        _ => {
                            if let Some(source) = self.source_ref(root) {
                                if !output.sources.iter().any(|f| f.source == source) {
                                    output.sources.push(SourceFact {
                                        source,
                                        path: summary::path(steps),
                                    });
                                }
                            }
                        }
                    }
                }
                summary.add_write(Access::slot(slot), output);
            }
        }
        (flows, summary)
    }

    /// A sink's search: a flow per source found (the first), and the
    /// inputs reaching it for the summary.
    fn collect_sink(
        &self,
        found: &Found,
        sink: MarkRef,
        tail: &[Step],
        flows: &mut Vec<InterFlow>,
        summary: &mut Summary,
    ) {
        let mut flowed = false;
        for (root, def) in &found.roots {
            let mut steps = self.chain(found, root, *def);
            steps.extend_from_slice(tail);
            match root {
                Root::Input(access) => summary.add_sink(
                    access.clone(),
                    SinkFact {
                        sink,
                        path: summary::path(steps),
                    },
                ),
                _ if !flowed => {
                    if let Some(source) = self.source_ref(root) {
                        flowed = true;
                        flows.push(InterFlow {
                            source,
                            sink,
                            path: steps,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    /// Storage whose final value callers see: each parameter's and the
    /// receiver's object when the function writes into it, and each shared
    /// variable it writes.
    fn outputs(&self) -> Vec<(Slot, Place)> {
        let rd = self.rd();
        let mut outputs: Vec<(Slot, Place)> = Vec::new();
        let written = |base: &Var| {
            rd.defs.iter().any(|def| {
                def.place.base == *base
                    && !matches!(def.origin, DefOrigin::Entry)
                    && !matches!(def.origin, DefOrigin::Extra(i) if matches!(self.extra_kinds[i], ExtraKind::Shared(_)))
            })
        };
        for (index, param) in self.func.params.iter().enumerate() {
            if written(param) {
                outputs.push((Slot::Param(index), Place::var(param.clone())));
            }
        }
        if let Some(receiver) = &self.func.receiver {
            if written(receiver) {
                outputs.push((Slot::Receiver, Place::var(receiver.clone())));
            }
        }
        let mut shared: Vec<&Var> = rd
            .defs
            .iter()
            .map(|def| &def.place.base)
            .filter(|base| shared_key(base).is_some())
            .collect();
        shared.sort();
        shared.dedup();
        for var in shared {
            if written(var) {
                let key = shared_key(var).expect("shared").to_string();
                outputs.push((Slot::Global(key), Place::var(var.clone())));
            }
        }
        outputs
    }

    /// Whether a parameter (or the receiver) was rebound to other storage
    /// on some path to an exit: its writes then no longer reach the
    /// caller's object. Shared variables are the storage itself.
    fn rebound_at_exit(&mut self, place: &Place, exits: &[usize]) -> bool {
        if shared_key(&place.base).is_some() {
            return false;
        }
        exits.iter().any(|&at| {
            let live = self.rd().live_before(at);
            self.rd().overlapping(&live, place).into_iter().any(|def| {
                let def = &self.rd().defs[def];
                def.strong && def.place == *place && !matches!(def.origin, DefOrigin::Entry)
            })
        })
    }
}

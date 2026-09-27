//! Flow-sensitive intraprocedural taint over one [`IrFunction`].
//!
//! A rule marks syntax — the values that are sources, the values a sink
//! must not receive, sanitized values, and extra propagation steps — by
//! source span ([`Mark`]); the rules-driven lowering recorded the IR value
//! of every expression ([`crate::ir::ExprValue`]), so each mark becomes a
//! definition in [`crate::reaching_defs`]:
//!
//! - a mark whose value an op computes (a call's result) taints or cleans
//!   that op's definition;
//! - a mark naming storage passed to a call (C `fgets(buf, …)`) defines
//!   that storage right after the call;
//! - a mark naming a parameter or variable holds from that point on.
//!
//! A sink is tainted when a definition reaching its value depends —
//! through the ops' data flow, the library models of
//! [`PropagationRules`], and the rule's propagators — on a source, with no
//! sanitized definition on the way. The search runs backward from each
//! sink over def-use chains, so its cost follows the sinks, and the path
//! it finds (source → each hop → sink) is the finding's evidence.
//!
//! Calls the library table does not model propagate by default: the result
//! carries the receiver's and arguments' data, and nothing else is
//! written. Phase 2 replaces that guess with summaries of the callee.

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet, VecDeque};

use crate::ir::{ExprValue, IrFunction, IrOp, Operand, Place, Span, Var};
use crate::propagation_rules::PropagationRules;
use crate::reaching_defs::{BitSet, DefId, DefOrigin, ExtraDef, Options, ReachingDefs, is_temp};

/// Functions larger than this many ops are not analyzed (generated code).
pub const MAX_OPS: usize = 50_000;

/// A syntax node a rule marks: its byte range, and its kind when known
/// (to tell apart nodes sharing a range, like `(x)` and `x`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pub start_byte: usize,
    pub end_byte: usize,
    pub kind: Option<&'static str>,
}

/// A rule's extra propagation step: `from`'s data flows into `to`.
#[derive(Debug, Clone, Copy)]
pub struct Propagator {
    pub from: Mark,
    pub to: Mark,
}

/// What one rule marks in one function.
#[derive(Debug, Clone, Default)]
pub struct TaintSpec {
    pub sources: Vec<Mark>,
    pub sanitizers: Vec<Mark>,
    pub sinks: Vec<Mark>,
    pub propagators: Vec<Propagator>,
}

/// A step of a flow: where a definition on the path was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hop {
    pub span: Span,
    /// The op that made it (`None`: at entry).
    pub op: Option<usize>,
}

/// A tainted sink: which source reaches which sink, through which steps
/// (source first; the sink itself is not a hop).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flow {
    /// Index into [`TaintSpec::sources`].
    pub source: usize,
    /// Index into [`TaintSpec::sinks`].
    pub sink: usize,
    pub hops: Vec<Hop>,
}

/// Run `spec` over `func`. `call_names(op)` names what the `Call` op at
/// `op` resolves to (qualified names from the index; empty when it does
/// not know), used with the callee as written to find library models.
pub fn analyze(
    func: &IrFunction,
    language: &str,
    spec: &TaintSpec,
    call_names: &dyn Fn(usize) -> Vec<String>,
) -> Vec<Flow> {
    if func.body.len() > MAX_OPS || spec.sources.is_empty() || spec.sinks.is_empty() {
        return Vec::new();
    }
    let rules = PropagationRules::for_language(language);
    let mut engine = Engine::new(func, rules, call_names);
    engine.mark(spec);
    engine.solve(spec)
}

/// What a call's result carries.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ResultRule {
    /// Its receiver's and arguments' data.
    Default,
    /// Nothing.
    Clean,
    /// The receiver's element under a constant key (or the whole receiver).
    KeyedRead(Option<String>),
}

/// The library model of one call.
#[derive(Debug, Clone)]
struct CallModel {
    result: ResultRule,
    /// The receiver takes its arguments' data.
    receiver_from_args: bool,
}

/// A read a definition's value depends on.
#[derive(Debug, Clone)]
enum Read {
    Operand(Operand),
    Place(Place),
}

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
}

struct Engine<'f> {
    func: &'f IrFunction,
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
    /// Sinks: (spec index, operand, point).
    sinks: Vec<(usize, Operand, usize)>,
    rd: Option<ReachingDefs>,
    live: HashMap<usize, BitSet>,
}

/// The last name of a callee or qualified name (`a.b.c` → `c`,
/// `x::Y::z` → `z`, `new Foo` → `Foo`).
fn last_name(text: &str) -> &str {
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

impl<'f> Engine<'f> {
    fn new(
        func: &'f IrFunction,
        rules: &PropagationRules,
        call_names: &dyn Fn(usize) -> Vec<String>,
    ) -> Self {
        let mut temp_defs = HashMap::new();
        let mut models = HashMap::new();
        let mut extras = Vec::new();
        let mut extra_kinds = Vec::new();
        let mut fluent_roots: HashMap<usize, Place> = HashMap::new();
        for (index, op) in func.body.iter().enumerate() {
            match op {
                IrOp::Assign { dst, .. }
                | IrOp::BinOp { dst, .. }
                | IrOp::FieldRead { dst, .. }
                    if is_temp(dst) =>
                {
                    temp_defs.insert(dst, index);
                }
                IrOp::Call {
                    dst,
                    callee,
                    receiver,
                    args,
                } => {
                    if let Some(dst) = dst.as_ref().filter(|dst| is_temp(dst)) {
                        temp_defs.insert(dst, index);
                    }
                    let mut names = call_names(index);
                    names.push(callee.clone());
                    let names: Vec<&str> = names.iter().map(|name| last_name(name)).collect();
                    let find = |test: &dyn Fn(&str) -> bool| names.iter().any(|name| test(name));
                    let mut model = CallModel {
                        result: ResultRule::Default,
                        receiver_from_args: find(&|name| rules.is_receiver_write(name)),
                    };
                    if find(&|name| rules.is_clean_result(name)) {
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
                            Operand::Var(var) => temp_defs.get(var),
                            _ => None,
                        })
                        .and_then(|op| fluent_roots.get(op))
                        .cloned();
                    if model.receiver_from_args {
                        if let Some(root) = receiver_place.clone().or(chained_root) {
                            fluent_roots.insert(index, root.clone());
                            if receiver_place.is_none() {
                                extras.push(ExtraDef {
                                    after: Some(index),
                                    place: root,
                                    strong: false,
                                });
                                extra_kinds.push(ExtraKind::From(
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
                            extras.push(ExtraDef {
                                after: Some(index),
                                place,
                                strong,
                            });
                            extra_kinds
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
                            extras.push(ExtraDef {
                                after: Some(index),
                                place,
                                strong: false,
                            });
                            extra_kinds.push(ExtraKind::From(reads));
                        }
                    }
                    models.insert(index, model);
                }
                _ => {}
            }
        }
        Self {
            func,
            models,
            temp_defs,
            extras,
            extra_kinds,
            source_ops: HashMap::new(),
            clean_ops: HashSet::new(),
            op_inputs: HashMap::new(),
            sinks: Vec::new(),
            rd: None,
            live: HashMap::new(),
        }
    }

    /// The recorded value of the node `mark` names: the value with its
    /// exact range (of its kind, when several), else the smallest one
    /// around it.
    fn locate(&self, mark: &Mark) -> Option<&'f ExprValue> {
        let values = &self.func.values;
        let exact: Vec<&ExprValue> = values
            .iter()
            .filter(|v| v.span.start_byte == mark.start_byte && v.span.end_byte == mark.end_byte)
            .collect();
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

    /// The call `value` is an argument (or receiver) of: the smallest
    /// value around it that a `Call` op computes.
    fn enclosing_call(&self, value: &ExprValue) -> Option<usize> {
        self.func
            .values
            .iter()
            .filter(|v| {
                v.span.contains(&value.span) && (v.span != value.span || v.kind != value.kind)
            })
            .filter_map(|v| {
                let op = self.defining_op(&v.operand)?;
                matches!(self.func.body[op], IrOp::Call { .. })
                    .then_some((v.span.end_byte - v.span.start_byte, op))
            })
            .min()
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

    fn mark(&mut self, spec: &TaintSpec) {
        for (index, mark) in spec.sources.iter().enumerate() {
            match self.target(mark) {
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
        for (index, mark) in spec.sinks.iter().enumerate() {
            if let Some(value) = self.locate(mark) {
                self.sinks.push((index, value.operand.clone(), value.at));
            }
        }
    }

    fn rd(&self) -> &ReachingDefs {
        self.rd.as_ref().expect("solved")
    }

    /// Definitions reaching a read at the point before op `at`.
    fn resolve(&mut self, read: &Read, at: usize) -> Vec<DefId> {
        // An operand is its variable (a temporary holding a field read is
        // that read's definition, whose input is the field's place).
        let place = match read {
            Read::Place(place) => Some(place.clone()),
            Read::Operand(Operand::Var(var)) => Some(Place::var(var.clone())),
            Read::Operand(_) => None,
        };
        let Some(place) = place else {
            return Vec::new();
        };
        if !self.live.contains_key(&at) {
            let live = self.rd().live_before(at);
            self.live.insert(at, live);
        }
        let live = &self.live[&at];
        self.rd().overlapping(live, &place)
    }

    /// What a definition's value is computed from.
    fn inputs(&self, def: DefId) -> Vec<(Read, usize)> {
        let rd = self.rd();
        let the_def = &rd.defs[def];
        let op = match the_def.origin {
            DefOrigin::Entry => return Vec::new(),
            DefOrigin::Extra(index) => {
                return match &self.extra_kinds[index] {
                    ExtraKind::From(reads) => reads.clone(),
                    ExtraKind::Source(_) | ExtraKind::Clean => Vec::new(),
                };
            }
            DefOrigin::Op(op) => op,
        };
        let operand = |operand: &Operand| (Read::Operand(operand.clone()), op);
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
                        vec![(Read::Place(place), op)]
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
                        Some(ResultRule::Clean) => Vec::new(),
                        Some(ResultRule::KeyedRead(key)) => {
                            let receiver_place = self
                                .func
                                .call_places(op)
                                .and_then(|p| p.receiver.clone())
                                .or_else(|| receiver.as_ref().and_then(|r| rd.place_of(r)));
                            match (receiver_place, key) {
                                (Some(place), Some(key)) => {
                                    vec![(Read::Place(place.field(key).0), op)]
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
            reads.extend(extra.iter().cloned());
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

    fn source_of(&self, def: DefId) -> Option<usize> {
        match self.rd().defs[def].origin {
            DefOrigin::Op(op) => {
                let source = *self.source_ops.get(&op)?;
                self.is_own_result(def, op).then_some(source)
            }
            DefOrigin::Extra(index) => match self.extra_kinds[index] {
                ExtraKind::Source(source) => Some(source),
                _ => None,
            },
            DefOrigin::Entry => None,
        }
    }

    fn hop(&self, def: DefId) -> Hop {
        let rd = self.rd();
        match rd.defs[def].origin {
            DefOrigin::Op(op) => Hop {
                span: self.func.span(op),
                op: Some(op),
            },
            DefOrigin::Extra(index) => match self.extras[index].after {
                Some(op) => Hop {
                    span: self.func.span(op),
                    op: Some(op),
                },
                None => self.entry_hop(&rd.defs[def].place),
            },
            DefOrigin::Entry => self.entry_hop(&rd.defs[def].place),
        }
    }

    fn entry_hop(&self, place: &Place) -> Hop {
        let span = self
            .func
            .params
            .iter()
            .position(|param| *param == place.base)
            .and_then(|index| self.func.param_spans.get(index).copied())
            .unwrap_or_default();
        Hop { span, op: None }
    }

    fn solve(&mut self, spec: &TaintSpec) -> Vec<Flow> {
        if self.sinks.is_empty() || (self.source_ops.is_empty() && self.extras.is_empty()) {
            return Vec::new();
        }
        self.rd = Some(ReachingDefs::compute(
            self.func,
            &self.extras,
            Options::default(),
        ));
        let mut flows = Vec::new();
        let sinks = std::mem::take(&mut self.sinks);
        for (sink, operand, at) in sinks {
            if !self.rd().is_reachable(at) {
                continue;
            }
            if let Some((source, hops)) = self.search(&operand, at) {
                flows.push(Flow { source, sink, hops });
            }
        }
        debug_assert!(flows.iter().all(|f| f.source < spec.sources.len()));
        flows
    }

    /// Backward breadth-first search from the value at a sink to a source.
    fn search(&mut self, operand: &Operand, at: usize) -> Option<(usize, Vec<Hop>)> {
        let start = self.resolve(&Read::Operand(operand.clone()), at);
        let mut toward_sink: HashMap<DefId, Option<DefId>> = HashMap::new();
        let mut queue: VecDeque<DefId> = VecDeque::new();
        for def in start {
            if toward_sink.insert(def, None).is_none() {
                queue.push_back(def);
            }
        }
        while let Some(def) = queue.pop_front() {
            if self.is_clean(def) {
                continue;
            }
            if let Some(source) = self.source_of(def) {
                let mut hops = Vec::new();
                let mut current = Some(def);
                while let Some(d) = current {
                    hops.push(self.hop(d));
                    current = toward_sink[&d];
                }
                return Some((source, hops));
            }
            for (read, point) in self.inputs(def) {
                for next in self.resolve(&read, point) {
                    if let std::collections::hash_map::Entry::Vacant(entry) =
                        toward_sink.entry(next)
                    {
                        entry.insert(Some(def));
                        queue.push_back(next);
                    }
                }
            }
        }
        None
    }
}

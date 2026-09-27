//! What a function does with tainted data, as its callers see it.
//!
//! A [`Summary`] is stated over the function's *inputs* ([`Access`]: a
//! parameter, the receiver, or shared storage — a field or global, see
//! [`crate::ir::shared`] — and a field path below it): which of them its
//! return value carries, which it writes into storage its callers see,
//! which reach a sink, and which sources it hands back. A caller applies
//! it at a call by mapping each input onto the call's arguments.
//!
//! Every fact carries its [`Path`] inside the function (and the functions
//! it calls), so a finding assembled from summaries shows every hop from
//! the source to the sink, across files.

use std::sync::Arc;

use crate::ir::Span;

/// A function of the analysed program, in the caller's numbering.
pub type FuncId = u32;

/// Where a value enters a function from its callers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Slot {
    /// The implicit object parameter (`self`, a Go receiver).
    Receiver,
    /// The `i`-th positional parameter.
    Param(usize),
    /// Shared storage — a class field or a global — by its key.
    Global(String),
}

/// A place relative to a slot: `param0`, `param1.name`, `@Cls.data[k]`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Access {
    pub slot: Slot,
    pub fields: Vec<String>,
}

impl Access {
    pub fn slot(slot: Slot) -> Self {
        Self {
            slot,
            fields: Vec::new(),
        }
    }
}

/// One step of a path: a definition, call or sink in some function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Step {
    pub func: FuncId,
    pub span: Span,
    /// The op that made it (`None`: at the function's entry).
    pub op: Option<usize>,
}

/// A rule mark (a source or a sink) in some function: its index in that
/// function's [`super::TaintSpec`] list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MarkRef {
    pub func: FuncId,
    pub index: usize,
}

/// The steps of a flow, first to last. Shared between the summaries that
/// extend it.
pub type Path = Arc<[Step]>;

/// Longest path kept: longer ones keep their first and last steps.
pub const MAX_PATH: usize = 48;

/// A path from `steps`, trimmed to [`MAX_PATH`].
pub fn path(steps: Vec<Step>) -> Path {
    if steps.len() <= MAX_PATH {
        return steps.into();
    }
    let head = MAX_PATH / 2;
    let tail = MAX_PATH - head;
    let mut kept = steps[..head].to_vec();
    kept.extend_from_slice(&steps[steps.len() - tail..]);
    kept.into()
}

/// A source whose value reaches some output: the source, and the path from
/// it to that output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFact {
    pub source: MarkRef,
    pub path: Path,
}

/// A sink an input reaches: the sink, and the path from the input's entry
/// to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinkFact {
    pub sink: MarkRef,
    pub path: Path,
}

/// What one output of a function carries: inputs (each with the path from
/// its entry to the output) and sources.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Output {
    pub inputs: Vec<(Access, Path)>,
    pub sources: Vec<SourceFact>,
}

impl Output {
    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty() && self.sources.is_empty()
    }

    fn add_input(&mut self, access: Access, path: Path) {
        if !self.inputs.iter().any(|(a, _)| *a == access) {
            self.inputs.push((access, path));
        }
    }

    fn add_source(&mut self, fact: SourceFact) {
        if !self.sources.iter().any(|f| f.source == fact.source) {
            self.sources.push(fact);
        }
    }

    fn merge(&mut self, other: &Output) {
        for (access, path) in &other.inputs {
            self.add_input(access.clone(), path.clone());
        }
        for fact in &other.sources {
            self.add_source(fact.clone());
        }
    }
}

/// A function's effect on tainted data, over its inputs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    /// What its return value carries.
    pub returns: Output,
    /// Storage its callers see that it writes: the receiver or a
    /// parameter's object (its fields, elements, what it points to), and
    /// shared storage.
    pub writes: Vec<(Access, Output)>,
    /// Inputs that reach a sink, in it or in a function it calls.
    pub sinks: Vec<(Access, SinkFact)>,
    /// Built from a guessed call target (a fallback, not the index):
    /// findings through it are less certain.
    pub guessed: bool,
}

/// At most this many sink facts per input (distinct sinks).
const MAX_SINKS_PER_INPUT: usize = 8;

impl Summary {
    /// Whether the function moves no data its callers could see.
    pub fn is_empty(&self) -> bool {
        self.returns.is_empty() && self.writes.is_empty() && self.sinks.is_empty()
    }

    pub(super) fn add_write(&mut self, target: Access, output: Output) {
        if output.is_empty() {
            return;
        }
        match self.writes.iter_mut().find(|(t, _)| *t == target) {
            Some((_, existing)) => existing.merge(&output),
            None => self.writes.push((target, output)),
        }
    }

    pub(super) fn add_sink(&mut self, input: Access, fact: SinkFact) {
        let same_input = self.sinks.iter().filter(|(a, _)| *a == input).count();
        let known = self
            .sinks
            .iter()
            .any(|(a, f)| *a == input && f.sink == fact.sink);
        if !known && same_input < MAX_SINKS_PER_INPUT {
            self.sinks.push((input, fact));
        }
    }

    pub(super) fn add_return_input(&mut self, access: Access, path: Path) {
        self.returns.add_input(access, path);
    }

    pub(super) fn add_return_source(&mut self, fact: SourceFact) {
        self.returns.add_source(fact);
    }

    /// The union of several targets' summaries (a dynamic dispatch).
    pub fn union<'a>(summaries: impl IntoIterator<Item = &'a Summary>) -> Summary {
        let mut out = Summary::default();
        for summary in summaries {
            out.returns.merge(&summary.returns);
            for (target, output) in &summary.writes {
                out.add_write(target.clone(), output.clone());
            }
            for (input, fact) in &summary.sinks {
                out.add_sink(input.clone(), fact.clone());
            }
            out.guessed |= summary.guessed;
        }
        out
    }

    /// The facts without their paths: what a fixpoint compares.
    pub fn shape(&self) -> Shape {
        let mut returns: Vec<String> = self
            .returns
            .inputs
            .iter()
            .map(|(a, _)| format!("{a:?}"))
            .chain(
                self.returns
                    .sources
                    .iter()
                    .map(|f| format!("{:?}", f.source)),
            )
            .collect();
        returns.sort();
        let mut writes: Vec<String> = self
            .writes
            .iter()
            .flat_map(|(target, output)| {
                output
                    .inputs
                    .iter()
                    .map(move |(a, _)| format!("{target:?}<-{a:?}"))
                    .chain(
                        output
                            .sources
                            .iter()
                            .map(move |f| format!("{target:?}<-{:?}", f.source)),
                    )
            })
            .collect();
        writes.sort();
        let mut sinks: Vec<String> = self
            .sinks
            .iter()
            .map(|(a, f)| format!("{a:?}->{:?}", f.sink))
            .collect();
        sinks.sort();
        Shape {
            returns,
            writes,
            sinks,
        }
    }
}

/// A summary's facts, paths left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shape {
    returns: Vec<String>,
    writes: Vec<String>,
    sinks: Vec<String>,
}

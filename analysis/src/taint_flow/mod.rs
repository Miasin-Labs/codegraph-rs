//! Flow-sensitive taint over [`IrFunction`]s, within a function and across
//! the calls between them.
//!
//! A rule marks syntax — the values that are sources, the values a sink
//! must not receive, sanitized values, extra propagation steps and
//! validation guards — by source span ([`Mark`]); the rules-driven lowering
//! recorded the IR value of every expression ([`crate::ir::ExprValue`]), so
//! each mark becomes a definition in [`crate::reaching_defs`]:
//!
//! - a mark whose value an op computes (a call's result) taints or cleans
//!   that op's definition;
//! - a mark naming storage passed to a call (C `fgets(buf, …)`) defines
//!   that storage right after the call;
//! - a mark naming a parameter or variable holds from that point on;
//! - a guard (`if (x < MAX)`) cleans reads of the value it checks on the
//!   branch where it is safe ([`guards`]).
//!
//! A sink is tainted when a definition reaching its value depends —
//! through the ops' data flow, the library models of [`PropagationRules`]
//! (with position-aware local lists, [`lists`]), the rule's propagators and
//! the summaries of the project functions it calls — on a source, with no
//! sanitized definition on the way. The search runs backward from each
//! sink over def-use chains, so its cost follows the sinks, and the path
//! it finds (source → each hop → sink, across functions) is the finding's
//! evidence.
//!
//! **Interprocedurally** ([`program`]), every function also gets a
//! [`Summary`] over its inputs — what its return value carries, what it
//! writes that callers see, which inputs reach a sink, which sources it
//! hands back — computed bottom-up over the call graph's strongly
//! connected components (recursion iterates to a bounded fixpoint). A call
//! into project code applies its callee's summary; one without a summary
//! (not lowered, over budget) carries nothing. Calls the library table
//! does not model propagate by default: the result carries the receiver's
//! and arguments' data, and nothing else is written.

mod engine;
mod guards;
mod lists;
pub mod program;
pub mod summary;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_inter;

use std::sync::Arc;
use std::time::Instant;

pub use summary::{
    Access,
    FuncId,
    MarkRef,
    Output,
    Path,
    SinkFact,
    Slot,
    SourceFact,
    Step,
    Summary,
};

use crate::ir::{IrFunction, Span};
use crate::propagation_rules::PropagationRules;

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

/// A validation guard: the condition `check` tests `value`; on the branch
/// where the check holds (`safe_when_true`) or fails (else), reads of the
/// value it dominates are clean.
#[derive(Debug, Clone, Copy)]
pub struct GuardMark {
    pub check: Mark,
    pub value: Mark,
    pub safe_when_true: bool,
}

/// What one rule marks in one function.
#[derive(Debug, Clone, Default)]
pub struct TaintSpec {
    pub sources: Vec<Mark>,
    pub sanitizers: Vec<Mark>,
    pub sinks: Vec<Mark>,
    pub propagators: Vec<Propagator>,
    pub guards: Vec<GuardMark>,
}

impl TaintSpec {
    /// Whether the rule marks nothing here.
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
            && self.sanitizers.is_empty()
            && self.sinks.is_empty()
            && self.propagators.is_empty()
            && self.guards.is_empty()
    }
}

/// A step of an intraprocedural flow: where a definition on the path was
/// made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hop {
    pub span: Span,
    /// The op that made it (`None`: at entry).
    pub op: Option<usize>,
}

/// A tainted sink within one function: which source reaches which sink,
/// through which steps (source first; the sink itself is not a hop).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flow {
    /// Index into [`TaintSpec::sources`].
    pub source: usize,
    /// Index into [`TaintSpec::sinks`].
    pub sink: usize,
    pub hops: Vec<Hop>,
}

/// A tainted sink, possibly across functions: the source and sink marks
/// (each in the function that holds it) and every step between, the sink
/// last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterFlow {
    pub source: MarkRef,
    pub sink: MarkRef,
    pub path: Vec<Step>,
}

/// What a `Call` op resolves to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallResolution {
    /// Qualified names of its targets (empty when unknown); with the
    /// callee as written, they select the library model.
    pub names: Vec<String>,
    /// It runs the project's own code: library models do not apply, and
    /// without a summary nothing is assumed to flow through it.
    pub in_project: bool,
    /// What the callee does, when known.
    pub summary: Option<Arc<Summary>>,
}

/// Work left for an analysis: search steps and a deadline. Spent, the
/// analysis stops where it is and says so.
#[derive(Debug, Clone)]
pub struct Budget {
    steps: usize,
    deadline: Option<Instant>,
    spent_count: usize,
    exhausted: bool,
}

impl Budget {
    pub fn new(steps: usize, deadline: Option<Instant>) -> Self {
        Self {
            steps,
            deadline,
            spent_count: 0,
            exhausted: false,
        }
    }

    /// No limit (tests, `--check` examples).
    pub fn unlimited() -> Self {
        Self::new(usize::MAX, None)
    }

    /// Spend `n` steps; `false` once the budget or the deadline is spent.
    pub fn spend(&mut self, n: usize) -> bool {
        if self.exhausted {
            return false;
        }
        self.steps = self.steps.saturating_sub(n);
        self.spent_count += n;
        if self.steps == 0 {
            self.exhausted = true;
        } else if self.spent_count >= 4096 {
            self.spent_count = 0;
            if self
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                self.exhausted = true;
            }
        }
        !self.exhausted
    }

    /// Whether the budget ran out.
    pub fn is_exhausted(&self) -> bool {
        self.exhausted
    }

    /// Check the deadline now (for work outside the search: matching,
    /// lowering); past it, the budget is spent.
    pub fn check_deadline(&mut self) -> bool {
        if !self.exhausted
            && self
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.exhausted = true;
        }
        !self.exhausted
    }
}

/// Run `spec` over `func` alone: the flows from its sources to its sinks.
/// `resolve(op)` says what the `Call` op at `op` resolves to.
pub fn analyze(
    func: &IrFunction,
    language: &str,
    spec: &TaintSpec,
    resolve: &dyn Fn(usize) -> CallResolution,
) -> Vec<Flow> {
    if spec.sources.is_empty() || spec.sinks.is_empty() {
        return Vec::new();
    }
    let (flows, _) = analyze_function(func, 0, language, spec, resolve, &mut Budget::unlimited());
    flows
        .into_iter()
        .filter(|flow| flow.source.func == 0 && flow.sink.func == 0)
        .map(|flow| {
            let mut steps = flow.path;
            steps.pop();
            Flow {
                source: flow.source.index,
                sink: flow.sink.index,
                hops: steps
                    .into_iter()
                    .map(|step| Hop {
                        span: step.span,
                        op: step.op,
                    })
                    .collect(),
            }
        })
        .collect()
}

/// Run `spec` over function `id`: the flows found in it (sources and sinks
/// possibly in the functions it calls, through their summaries) and its
/// own summary. `None` summary: too large to analyze.
pub fn analyze_function(
    func: &IrFunction,
    id: FuncId,
    language: &str,
    spec: &TaintSpec,
    resolve: &dyn Fn(usize) -> CallResolution,
    budget: &mut Budget,
) -> (Vec<InterFlow>, Option<Summary>) {
    if func.body.len() > MAX_OPS || !budget.spend(func.body.len().max(1)) {
        return (Vec::new(), None);
    }
    let rules = PropagationRules::for_language(language);
    let mut engine = engine::Engine::new(func, id, rules, resolve);
    engine.mark(spec);
    engine.solve();
    let (flows, summary) = engine.run(budget);
    (flows, Some(summary))
}

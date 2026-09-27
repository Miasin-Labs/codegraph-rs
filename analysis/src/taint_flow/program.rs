//! Taint over a whole program: functions, the calls between them, and a
//! rule's marks in each, solved bottom-up over the call graph's strongly
//! connected components.
//!
//! Summaries depend on the rule — its sanitizers cut paths, its sources
//! and sinks are what they report — so a [`Solver`] computes them per rule
//! for the functions a rule *touches* (it marks something in them or in a
//! function they call, transitively). Every other function gets its
//! *pure* summary, computed once with no marks and shared by every rule:
//! how data moves from its inputs to its return value and to what its
//! callers see, through library models and its callees' pure summaries.
//! Within a component (recursion), summaries start empty and are recomputed
//! until they stop changing, at most [`MAX_ROUNDS`] times.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::{Budget, CallResolution, FuncId, InterFlow, Summary, TaintSpec, analyze_function};
use crate::ir::IrFunction;

/// Rounds of a recursive component's fixpoint.
pub const MAX_ROUNDS: usize = 4;

/// What one call op of a function resolves to.
#[derive(Debug, Clone, Default)]
pub struct CallTargets {
    /// Qualified names (for library models, when not project code).
    pub names: Vec<String>,
    /// The program's functions it may run (several: a dynamic dispatch).
    pub targets: Vec<FuncId>,
    /// Project code, even when no target is in the program.
    pub in_project: bool,
    /// The targets are a guess (no call edge in the index): findings
    /// through them are less certain.
    pub guessed: bool,
}

/// A function of the program.
pub struct Function {
    pub ir: Arc<IrFunction>,
    pub language: &'static str,
    /// Call op index → what it resolves to (ops absent: unresolved).
    pub calls: HashMap<usize, CallTargets>,
}

/// What a rule's run found.
#[derive(Debug, Default)]
pub struct Solution {
    pub flows: Vec<InterFlow>,
    /// Flows through a guessed call target.
    pub guessed: HashSet<usize>,
    /// The budget ran out: some functions were not analyzed.
    pub partial: bool,
}

/// The program's call graph, its components, and the pure summaries.
pub struct Solver<'p> {
    functions: &'p [Function],
    /// Components, callees before callers.
    sccs: Vec<Vec<FuncId>>,
    scc_of: Vec<usize>,
    callers: Vec<Vec<FuncId>>,
    pure: Vec<Option<Option<Arc<Summary>>>>,
}

impl<'p> Solver<'p> {
    pub fn new(functions: &'p [Function]) -> Self {
        let count = functions.len();
        let mut callers: Vec<Vec<FuncId>> = vec![Vec::new(); count];
        for (id, function) in functions.iter().enumerate() {
            for call in function.calls.values() {
                for &target in &call.targets {
                    let list = &mut callers[target as usize];
                    if !list.contains(&(id as FuncId)) {
                        list.push(id as FuncId);
                    }
                }
            }
        }
        let sccs = tarjan(functions);
        let mut scc_of = vec![0; count];
        for (index, scc) in sccs.iter().enumerate() {
            for &member in scc {
                scc_of[member as usize] = index;
            }
        }
        Self {
            functions,
            sccs,
            scc_of,
            callers,
            pure: vec![None; count],
        }
    }

    /// Run one rule, whose marks in each function are `specs` (functions
    /// absent mark nothing).
    pub fn run(&mut self, specs: &HashMap<FuncId, TaintSpec>, budget: &mut Budget) -> Solution {
        let mut solution = Solution::default();
        // Touched: marked, or calling a touched function.
        let mut touched = vec![false; self.functions.len()];
        let mut stack: Vec<FuncId> = specs
            .iter()
            .filter(|(_, spec)| !spec.is_empty())
            .map(|(&id, _)| id)
            .collect();
        while let Some(id) = stack.pop() {
            if std::mem::replace(&mut touched[id as usize], true) {
                continue;
            }
            stack.extend(self.callers[id as usize].iter().copied());
        }
        let empty = TaintSpec::default();
        let mut summaries: Vec<Option<Arc<Summary>>> = vec![None; self.functions.len()];
        for index in 0..self.sccs.len() {
            let scc = self.sccs[index].clone();
            if !scc.iter().any(|&id| touched[id as usize]) {
                continue;
            }
            if budget.is_exhausted() {
                solution.partial = true;
                break;
            }
            let recursive = scc.len() > 1 || self.calls_itself(scc[0]);
            if recursive {
                for &id in &scc {
                    summaries[id as usize] = Some(Arc::new(Summary::default()));
                }
            }
            let rounds = if recursive { MAX_ROUNDS } else { 1 };
            for round in 0..rounds {
                let mut changed = false;
                let last = round + 1 == rounds;
                let mut round_flows: Vec<(InterFlow, bool)> = Vec::new();
                for &id in &scc {
                    let spec = specs.get(&id).unwrap_or(&empty);
                    let (flows, summary, guessed) =
                        self.analyze(id, spec, &touched, &summaries, budget);
                    let summary = summary.map(Arc::new);
                    let before = summaries[id as usize].as_ref().map(|s| s.shape());
                    if before != summary.as_ref().map(|s| s.shape()) {
                        changed = true;
                    }
                    summaries[id as usize] = summary;
                    round_flows.extend(flows.into_iter().map(|flow| (flow, guessed)));
                }
                if !changed || last {
                    for (flow, guessed) in round_flows {
                        if guessed {
                            solution.guessed.insert(solution.flows.len());
                        }
                        solution.flows.push(flow);
                    }
                    break;
                }
            }
        }
        self.entry_flows(&summaries, &mut solution);
        solution.partial |= budget.is_exhausted();
        solution
    }

    /// Shared storage one function writes a source into reaches the sinks
    /// of the functions nothing in the program calls — handlers,
    /// destructors, callbacks — which may run after it (a C++ constructor
    /// storing input in a member its destructor uses). A function that has
    /// callers is only ever given what they store (its calls decide), so
    /// this adds nothing for it. Flagged as guesses: the order is assumed.
    fn entry_flows(&self, summaries: &[Option<Arc<Summary>>], solution: &mut Solution) {
        let mut written: HashMap<&str, &super::SourceFact> = HashMap::new();
        for summary in summaries.iter().flatten() {
            for (target, output) in &summary.writes {
                if let (super::Slot::Global(key), Some(fact)) =
                    (&target.slot, output.sources.first())
                {
                    written.entry(key.as_str()).or_insert(fact);
                }
            }
        }
        if written.is_empty() {
            return;
        }
        for (id, summary) in summaries.iter().enumerate() {
            let Some(summary) = summary else {
                continue;
            };
            if !self.callers[id].is_empty() {
                continue;
            }
            for (input, sink) in &summary.sinks {
                let super::Slot::Global(key) = &input.slot else {
                    continue;
                };
                let Some(fact) = written.get(key.as_str()) else {
                    continue;
                };
                let mut path: Vec<super::Step> = fact.path.to_vec();
                path.extend(sink.path.iter().copied());
                solution.guessed.insert(solution.flows.len());
                solution.flows.push(InterFlow {
                    source: fact.source,
                    sink: sink.sink,
                    path,
                });
            }
        }
    }

    fn calls_itself(&self, id: FuncId) -> bool {
        self.functions[id as usize]
            .calls
            .values()
            .any(|call| call.targets.contains(&id))
    }

    /// Analyze function `id` with `spec`; callees' summaries are the rule's
    /// where touched, else pure. Returns the flows, the summary, and
    /// whether a guessed target was used.
    fn analyze(
        &mut self,
        id: FuncId,
        spec: &TaintSpec,
        touched: &[bool],
        summaries: &[Option<Arc<Summary>>],
        budget: &mut Budget,
    ) -> (Vec<InterFlow>, Option<Summary>, bool) {
        let functions = self.functions;
        let function = &functions[id as usize];
        let mut resolved: HashMap<usize, CallResolution> = HashMap::new();
        let mut guessed = false;
        for (&op, call) in &function.calls {
            let mut resolution = CallResolution {
                names: call.names.clone(),
                in_project: call.in_project || !call.targets.is_empty(),
                summary: None,
            };
            let mut parts: Vec<Arc<Summary>> = Vec::new();
            let mut complete = !call.targets.is_empty();
            for &target in &call.targets {
                let summary = if touched[target as usize] {
                    summaries[target as usize].clone()
                } else {
                    self.pure(target, budget)
                };
                match summary {
                    Some(summary) => parts.push(summary),
                    None => complete = false,
                }
            }
            if complete {
                let mut summary = if parts.len() == 1 {
                    (*parts[0]).clone()
                } else {
                    Summary::union(parts.iter().map(|s| s.as_ref()))
                };
                summary.guessed |= call.guessed;
                guessed |= summary.guessed && !summary.is_empty();
                resolution.summary = Some(Arc::new(summary));
            }
            resolved.insert(op, resolution);
        }
        let resolve = |op: usize| resolved.get(&op).cloned().unwrap_or_default();
        let (flows, summary) =
            analyze_function(&function.ir, id, function.language, spec, &resolve, budget);
        (flows, summary, guessed)
    }

    /// The pure summary of `id` (and of everything it calls), computed on
    /// first use.
    fn pure(&mut self, id: FuncId, budget: &mut Budget) -> Option<Arc<Summary>> {
        if let Some(known) = &self.pure[id as usize] {
            return known.clone();
        }
        // The components `id` reaches, callees first.
        let mut needed: Vec<usize> = Vec::new();
        let mut seen: HashSet<FuncId> = HashSet::new();
        let mut stack = vec![id];
        while let Some(current) = stack.pop() {
            if !seen.insert(current) || self.pure[current as usize].is_some() {
                continue;
            }
            needed.push(self.scc_of[current as usize]);
            for call in self.functions[current as usize].calls.values() {
                stack.extend(call.targets.iter().copied());
            }
        }
        needed.sort_unstable();
        needed.dedup();
        let untouched = vec![false; self.functions.len()];
        let empty = TaintSpec::default();
        for index in needed {
            let scc = self.sccs[index].clone();
            let recursive = scc.len() > 1 || self.calls_itself(scc[0]);
            let mut current: Vec<Option<Arc<Summary>>> = vec![None; self.functions.len()];
            if recursive {
                for &member in &scc {
                    self.pure[member as usize] = Some(Some(Arc::new(Summary::default())));
                }
            }
            let rounds = if recursive { MAX_ROUNDS } else { 1 };
            for _ in 0..rounds {
                let mut changed = false;
                for &member in &scc {
                    let (_, summary, _) =
                        self.analyze(member, &empty, &untouched, &current, budget);
                    let summary = summary.map(Arc::new);
                    let before = self.pure[member as usize]
                        .as_ref()
                        .and_then(|s| s.as_ref().map(|s| s.shape()));
                    if before != summary.as_ref().map(|s| s.shape()) {
                        changed = true;
                    }
                    self.pure[member as usize] = Some(summary.clone());
                    current[member as usize] = summary;
                }
                if !changed {
                    break;
                }
            }
        }
        let found = self.pure[id as usize].clone().flatten();
        if budget.is_exhausted() {
            // What the spent budget left unanalyzed is unknown, not
            // summary-less: a later run with budget computes it.
            for slot in &mut self.pure {
                if matches!(slot, Some(None)) {
                    *slot = None;
                }
            }
        }
        found
    }
}

/// Strongly connected components of the call graph, callees first
/// (Tarjan's algorithm, iterative: depth is bounded by the input).
fn tarjan(functions: &[Function]) -> Vec<Vec<FuncId>> {
    let count = functions.len();
    let succs: Vec<Vec<FuncId>> = functions
        .iter()
        .map(|function| {
            let mut targets: Vec<FuncId> = function
                .calls
                .values()
                .flat_map(|call| call.targets.iter().copied())
                .filter(|&t| (t as usize) < count)
                .collect();
            targets.sort_unstable();
            targets.dedup();
            targets
        })
        .collect();
    let mut index = vec![usize::MAX; count];
    let mut low = vec![0usize; count];
    let mut on_stack = vec![false; count];
    let mut stack: Vec<FuncId> = Vec::new();
    let mut sccs: Vec<Vec<FuncId>> = Vec::new();
    let mut next = 0usize;
    for root in 0..count {
        if index[root] != usize::MAX {
            continue;
        }
        // (node, next successor position)
        let mut work: Vec<(usize, usize)> = vec![(root, 0)];
        index[root] = next;
        low[root] = next;
        next += 1;
        stack.push(root as FuncId);
        on_stack[root] = true;
        while let Some(&(node, position)) = work.last() {
            if position < succs[node].len() {
                let succ = succs[node][position] as usize;
                if let Some(top) = work.last_mut() {
                    top.1 += 1;
                }
                if index[succ] == usize::MAX {
                    index[succ] = next;
                    low[succ] = next;
                    next += 1;
                    stack.push(succ as FuncId);
                    on_stack[succ] = true;
                    work.push((succ, 0));
                } else if on_stack[succ] {
                    low[node] = low[node].min(index[succ]);
                }
                continue;
            }
            work.pop();
            if let Some(&(parent, _)) = work.last() {
                low[parent] = low[parent].min(low[node]);
            }
            if low[node] == index[node] {
                let mut scc = Vec::new();
                while let Some(member) = stack.pop() {
                    on_stack[member as usize] = false;
                    scc.push(member);
                    if member as usize == node {
                        break;
                    }
                }
                scc.sort_unstable();
                sccs.push(scc);
            }
        }
    }
    sccs
}

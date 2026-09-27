//! Taint rules: a finding is a sink whose value a source's value reaches —
//! in the same function or across the project's calls — with no sanitizer
//! or validation guard on the way.
//!
//! Each role's patterns match like check patterns (queries or weggli, with
//! their `where` predicates); their role captures then mark syntax, which
//! [`codegraph_analysis::taint_flow`] maps onto each function's IR (lowered
//! once per function by the rules-driven lowering) and solves over
//! flow-sensitive reaching definitions. Across functions it composes
//! per-function summaries bottom-up over the call graph
//! ([`codegraph_analysis::taint_flow::program`]); [`program`] builds that
//! graph — the functions holding a rule's marks and everything they call —
//! from the index's call edges (by line/col plus the last name,
//! [`Semantics::call_at`]) and, where the index has no edge, from the
//! syntax (constructors, receivers' allocations, interface
//! implementations, same-file names). Without an index (`--check`) calls
//! resolve within the example's file.
//!
//! A flow is reported at its sink, with every step — source, each
//! definition, each call crossed, in whichever file — as evidence.

mod facts;
mod headers;
mod program;

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use codegraph_analysis::taint_flow::program::Solver;
use codegraph_analysis::taint_flow::{
    Budget,
    FuncId,
    GuardMark,
    InterFlow,
    Mark,
    Propagator,
    TaintSpec,
};

use super::compile::{Pattern, Rule, TaintRule};
use super::engine::{self, FileInput, FileResult, Hit, Rejection, node_at, one_line, position};
use super::lang;
use super::semantics::Semantics;

/// Ops the program may lower, at most (the node budget).
pub(super) const MAX_PROGRAM_OPS: usize = 4_000_000;
/// Search steps a sweep may take, at most.
pub(super) const MAX_STEPS: usize = 50_000_000;

/// One step of a flow's evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TraceStep {
    pub file: String,
    pub line: u32,
    /// The line's code, on one line.
    pub code: String,
}

/// How a sink's value was reached: the source, then each step.
#[derive(Debug, Clone)]
pub(crate) struct Trace {
    pub source_file: String,
    pub source_line: u32,
    /// The source's code, on one line.
    pub source_code: String,
    /// The source pattern's label.
    pub source_pattern: String,
    /// The steps between source and sink, in order (each line once, the
    /// source's and sink's own lines left out).
    pub hops: Vec<TraceStep>,
    /// It crosses a call whose target was guessed (no index edge).
    pub guessed: bool,
}

/// A role match: its hit, the marked capture's range, and where it is.
struct Marked {
    hit: Hit,
    range: Range<usize>,
    label: String,
    file: usize,
    /// Its function's candidate (`None`: outside every function).
    candidate: Option<usize>,
}

/// A guard match: the checked value and the condition.
struct GuardMarked {
    value: Range<usize>,
    check: Range<usize>,
    safe_when_true: bool,
    file: usize,
    candidate: Option<usize>,
}

/// One rule's matches, by role.
#[derive(Default)]
struct RuleMarks {
    sinks: Vec<Marked>,
    sources: Vec<Marked>,
    sanitizers: Vec<Marked>,
    /// (from, to, file, candidate): both ends in the same function.
    propagators: Vec<(Range<usize>, Range<usize>, usize, usize)>,
    guards: Vec<GuardMarked>,
}

/// What a run of taint rules over files found.
pub(super) struct Outcome {
    /// Per rule, per file.
    pub results: Vec<Vec<FileResult>>,
    /// The budget ran out: some functions were not analyzed.
    pub partial: bool,
}

/// Run a taint rule over one file (rule examples, `--check`): calls resolve
/// within the file.
pub(super) fn run(
    rule: &Rule,
    taint: &TaintRule,
    file: &FileInput,
    semantics: &dyn Semantics,
    trace: bool,
) -> FileResult {
    let files = [file];
    let mut outcome = run_files(
        &[(rule, taint)],
        &files,
        semantics,
        &mut Budget::unlimited(),
        MAX_PROGRAM_OPS,
        trace,
    );
    outcome
        .results
        .pop()
        .and_then(|mut per_file| per_file.pop())
        .unwrap_or_default()
}

/// Run taint rules over `files` together: one program, one solver, each
/// rule's marks.
pub(super) fn run_files(
    rules: &[(&Rule, &TaintRule)],
    files: &[&FileInput],
    semantics: &dyn Semantics,
    budget: &mut Budget,
    max_ops: usize,
    trace: bool,
) -> Outcome {
    let started = std::time::Instant::now();
    let mut results: Vec<Vec<FileResult>> = rules
        .iter()
        .map(|_| files.iter().map(|_| FileResult::default()).collect())
        .collect();
    headers::link(files);
    let mut table = program::Table::new(files);
    let mut marks: Vec<RuleMarks> = Vec::new();
    let mut shared = SharedHits::default();
    for (index, (rule, taint)) in rules.iter().enumerate() {
        marks.push(collect_marks(
            rule,
            taint,
            &mut shared,
            files,
            &mut table,
            semantics,
            trace,
            &mut results[index],
        ));
    }
    // Seeds: every function a rule marks something in, then the other
    // functions of their files (callers holding no mark of their own: a
    // function that stores a value and calls the sink's).
    let mut seeds: Vec<usize> = Vec::new();
    let mut seen: HashSet<usize> = HashSet::new();
    let mut marked_files: HashSet<usize> = HashSet::new();
    for rule_marks in &marks {
        let marked = rule_marks
            .sinks
            .iter()
            .chain(&rule_marks.sources)
            .chain(&rule_marks.sanitizers);
        for m in marked {
            if let Some(candidate) = m.candidate {
                if seen.insert(candidate) {
                    seeds.push(candidate);
                }
            }
            marked_files.insert(m.file);
        }
    }
    for candidate in 0..table.candidates.len() {
        let c = &table.candidates[candidate];
        if !c.top_level && marked_files.contains(&c.file) && seen.insert(candidate) {
            seeds.push(candidate);
        }
    }
    let tracing = std::env::var_os("CODEGRAPH_TAINT_TRACE").is_some();
    let marked_at = std::time::Instant::now();
    table.include(&seeds, semantics, max_ops);
    let table = table;
    if tracing {
        trace_program(&table, files);
        eprintln!(
            "taint: marks {:?}, program {:?}",
            marked_at.duration_since(started),
            marked_at.elapsed()
        );
    }
    let mut solver = Solver::new(&table.functions);
    let mut partial = table.partial;
    for (index, ((rule, _), rule_marks)) in rules.iter().zip(&marks).enumerate() {
        if rule_marks.sinks.is_empty() {
            continue;
        }
        let specs = Specs::build(rule_marks, &table, files);
        let solve_at = std::time::Instant::now();
        let solution = solver.run(&specs.specs, budget);
        if tracing {
            eprintln!(
                "taint: {} solved in {:?}: {} flows",
                rule.id,
                solve_at.elapsed(),
                solution.flows.len()
            );
        }
        partial |= solution.partial;
        let mut reached: HashSet<usize> = HashSet::new();
        // The best flow per sink: sure before guessed, then shortest.
        let mut best: HashMap<usize, (bool, usize, &InterFlow)> = HashMap::new();
        for (flow_index, flow) in solution.flows.iter().enumerate() {
            let Some(&sink) = specs
                .sinks
                .get(&flow.sink.func)
                .and_then(|s| s.get(flow.sink.index))
            else {
                continue;
            };
            let guessed = solution.guessed.contains(&flow_index);
            let key = (guessed, flow.path.len());
            match best.get(&sink) {
                Some((g, len, _)) if (*g, *len) <= key => {}
                _ => {
                    best.insert(sink, (guessed, flow.path.len(), flow));
                }
            }
        }
        let mut sinks: Vec<usize> = best.keys().copied().collect();
        sinks.sort_unstable();
        for sink_index in sinks {
            let (guessed, _, flow) = best[&sink_index];
            let Some(&source_index) = specs
                .sources
                .get(&flow.source.func)
                .and_then(|s| s.get(flow.source.index))
            else {
                continue;
            };
            reached.insert(sink_index);
            let sink = &rule_marks.sinks[sink_index];
            let source = &rule_marks.sources[source_index];
            let hit = flow_hit(sink, source, flow, guessed, &table, files);
            results[index][sink.file].hits.push(hit);
        }
        if trace {
            reject_unreached(
                rule,
                rule_marks,
                &reached,
                &table,
                files,
                &mut results[index],
            );
        }
    }
    Outcome {
        results,
        partial: partial || budget.is_exhausted(),
    }
}

/// The candidate of the function a range sits in (the file's top-level
/// code outside every function); `None` when no node spans it.
fn candidate_of(
    table: &mut program::Table,
    file: usize,
    input: &FileInput,
    range: &Range<usize>,
) -> Option<usize> {
    let rules = lang::for_language(input.language);
    let root = input.tree.root_node();
    let node = node_at(root, range)?;
    let function = lang::enclosing_function(rules, node).unwrap_or(root);
    Some(table.candidate(file, function))
}

/// Every role's matches of one rule over the files. Without a sink
/// anywhere, the other roles are not matched.
#[allow(clippy::too_many_arguments)]
fn collect_marks(
    rule: &Rule,
    taint: &TaintRule,
    shared: &mut SharedHits,
    files: &[&FileInput],
    table: &mut program::Table,
    semantics: &dyn Semantics,
    trace: bool,
    results: &mut [FileResult],
) -> RuleMarks {
    let mut marks = RuleMarks::default();
    let marked = |hits: Vec<Hit>,
                  capture: &str,
                  pattern: &Pattern,
                  file: usize,
                  table: &mut program::Table|
     -> Vec<Marked> {
        hits.into_iter()
            .filter_map(|hit| {
                let range = hit.capture(capture)?.clone();
                let candidate = candidate_of(table, file, files[file], &range);
                Some(Marked {
                    hit,
                    range,
                    label: pattern.label.clone(),
                    file,
                    candidate,
                })
            })
            .collect()
    };
    for (file_index, file) in files.iter().enumerate() {
        for (index, pattern) in rule.checks.iter().enumerate() {
            let found = engine::pattern_hits(pattern, index, file, semantics, trace);
            results[file_index].rejected.extend(found.rejected);
            let sinks = marked(
                found.hits,
                &taint.sink_values[index],
                pattern,
                file_index,
                table,
            );
            marks.sinks.extend(sinks);
        }
    }
    if marks.sinks.is_empty() {
        return marks;
    }
    for (file_index, file) in files.iter().enumerate() {
        for (index, role) in taint.sources.iter().enumerate() {
            let hits = shared.hits(&role.pattern, index, file_index, file, semantics);
            let sources = marked(hits, &role.value, &role.pattern, file_index, table);
            marks.sources.extend(sources);
        }
        for (index, role) in taint.sanitizers.iter().enumerate() {
            let hits = shared.hits(&role.pattern, index, file_index, file, semantics);
            let sanitizers = marked(hits, &role.value, &role.pattern, file_index, table);
            marks.sanitizers.extend(sanitizers);
        }
        for (index, propagator) in taint.propagators.iter().enumerate() {
            for hit in shared.hits(&propagator.pattern, index, file_index, file, semantics) {
                let (Some(from), Some(to)) = (
                    hit.capture(&propagator.from).cloned(),
                    hit.capture(&propagator.to).cloned(),
                ) else {
                    continue;
                };
                if let Some(candidate) = candidate_of(table, file_index, file, &from) {
                    if candidate_of(table, file_index, file, &to) == Some(candidate) {
                        marks.propagators.push((from, to, file_index, candidate));
                    }
                }
            }
        }
        for (index, guard) in taint.guards.iter().enumerate() {
            for hit in shared.hits(&guard.pattern, index, file_index, file, semantics) {
                let (Some(value), Some(check)) = (
                    hit.capture(&guard.value).cloned(),
                    hit.capture(&guard.check).cloned(),
                ) else {
                    continue;
                };
                let candidate = candidate_of(table, file_index, file, &check);
                marks.guards.push(GuardMarked {
                    value,
                    check,
                    safe_when_true: guard.safe_when_true,
                    file: file_index,
                    candidate,
                });
            }
        }
    }
    marks
}

/// Role matches per file, shared by the rules whose patterns match the
/// same syntax (sources aliased across a language's rules): each such
/// pattern runs once per file.
#[derive(Default)]
struct SharedHits {
    hits: HashMap<(String, usize), Vec<Hit>>,
}

impl SharedHits {
    fn hits(
        &mut self,
        pattern: &Pattern,
        index: usize,
        file_index: usize,
        file: &FileInput,
        semantics: &dyn Semantics,
    ) -> Vec<Hit> {
        self.hits
            .entry((pattern.key.clone(), file_index))
            .or_insert_with(|| engine::pattern_hits(pattern, index, file, semantics, false).hits)
            .clone()
    }
}

/// A rule's marks as the solver takes them, per program function, and
/// back from each spec entry to its match.
struct Specs {
    specs: HashMap<FuncId, TaintSpec>,
    /// Function → its spec's sinks, as indices into the rule's sinks.
    sinks: HashMap<FuncId, Vec<usize>>,
    sources: HashMap<FuncId, Vec<usize>>,
}

impl Specs {
    fn build(marks: &RuleMarks, table: &program::Table, files: &[&FileInput]) -> Self {
        let mut out = Specs {
            specs: HashMap::new(),
            sinks: HashMap::new(),
            sources: HashMap::new(),
        };
        let mark = |file: usize, range: &Range<usize>| Mark {
            start_byte: range.start,
            end_byte: range.end,
            kind: node_at(files[file].tree.root_node(), range)
                .filter(|node| node.byte_range() == *range)
                .map(|node| node.kind()),
        };
        let id_of = |candidate: Option<usize>| candidate.and_then(|c| table.ids.get(&c).copied());
        for (index, sink) in marks.sinks.iter().enumerate() {
            if let Some(id) = id_of(sink.candidate) {
                out.specs
                    .entry(id)
                    .or_default()
                    .sinks
                    .push(mark(sink.file, &sink.range));
                out.sinks.entry(id).or_default().push(index);
            }
        }
        for (index, source) in marks.sources.iter().enumerate() {
            if let Some(id) = id_of(source.candidate) {
                out.specs
                    .entry(id)
                    .or_default()
                    .sources
                    .push(mark(source.file, &source.range));
                out.sources.entry(id).or_default().push(index);
            }
        }
        for sanitizer in &marks.sanitizers {
            if let Some(id) = id_of(sanitizer.candidate) {
                out.specs
                    .entry(id)
                    .or_default()
                    .sanitizers
                    .push(mark(sanitizer.file, &sanitizer.range));
            }
        }
        for (from, to, file, candidate) in &marks.propagators {
            if let Some(id) = id_of(Some(*candidate)) {
                out.specs
                    .entry(id)
                    .or_default()
                    .propagators
                    .push(Propagator {
                        from: mark(*file, from),
                        to: mark(*file, to),
                    });
            }
        }
        for guard in &marks.guards {
            if let Some(id) = id_of(guard.candidate) {
                out.specs.entry(id).or_default().guards.push(GuardMark {
                    check: mark(guard.file, &guard.check),
                    value: mark(guard.file, &guard.value),
                    safe_when_true: guard.safe_when_true,
                });
            }
        }
        out
    }
}

/// The hit a flow makes: the sink's match, with the source and every step
/// between as its trace.
fn flow_hit(
    sink: &Marked,
    source: &Marked,
    flow: &InterFlow,
    guessed: bool,
    table: &program::Table,
    files: &[&FileInput],
) -> Hit {
    let file_of =
        |func: FuncId| -> usize { table.candidates[table.candidate_of[func as usize]].file };
    let source_file = files[source.file];
    let sink_file = files[sink.file];
    let source_line = position(source_file.tree, source.range.start).0;
    let sink_line = position(sink_file.tree, sink.range.start).0;
    let mut hops: Vec<TraceStep> = Vec::new();
    let mut seen: HashSet<(usize, u32)> = HashSet::new();
    seen.insert((source.file, source_line));
    seen.insert((sink.file, sink_line));
    for step in &flow.path {
        let file = file_of(step.func);
        let line = step.span.line;
        if line == 0 || !seen.insert((file, line)) {
            continue;
        }
        hops.push(TraceStep {
            file: files[file].path.to_string(),
            line,
            code: one_line(files[file].line_text(line), 80),
        });
    }
    let mut hit = sink.hit.clone();
    hit.at = sink.range.clone();
    hit.flow = Some(Trace {
        source_file: source_file.path.to_string(),
        source_line,
        source_code: one_line(&source_file.source[source.range.clone()], 80),
        source_pattern: source.label.clone(),
        hops,
        guessed,
    });
    hit
}

/// Why each sink no source reached was not reported (`--check` traces).
fn reject_unreached(
    rule: &Rule,
    marks: &RuleMarks,
    reached: &HashSet<usize>,
    table: &program::Table,
    files: &[&FileInput],
    results: &mut [FileResult],
) {
    let _ = rule;
    for (index, sink) in marks.sinks.iter().enumerate() {
        if reached.contains(&index) {
            continue;
        }
        let line = position(files[sink.file].tree, sink.range.start).0;
        let reason = match sink.candidate {
            None => "it is not inside a function".to_string(),
            Some(candidate) => {
                let c = &table.candidates[candidate];
                let name = if c.top_level {
                    "the top-level code".to_string()
                } else {
                    format!("`{}`", c.name)
                };
                let in_function = marks
                    .sources
                    .iter()
                    .filter(|s| s.candidate == Some(candidate))
                    .count();
                if marks.sources.is_empty() {
                    format!("no source matched in {name}")
                } else if !table.ids.contains_key(&candidate) {
                    format!("{name} could not be lowered to IR")
                } else {
                    format!(
                        "no source reaches it ({in_function} source match{} in {name})",
                        if in_function == 1 { "" } else { "es" }
                    )
                }
            }
        };
        results[sink.file].rejected.push(Rejection {
            pattern: sink.label.clone(),
            line,
            reason,
        });
    }
}

/// `CODEGRAPH_TAINT_TRACE=1`: the program on stderr — each function and
/// what its calls resolve to.
fn trace_program(table: &program::Table, files: &[&FileInput]) {
    eprintln!(
        "taint: {} functions, {} ops{}",
        table.functions.len(),
        table.ops,
        if table.partial { " (partial)" } else { "" }
    );
    for (id, function) in table.functions.iter().enumerate() {
        let candidate = &table.candidates[table.candidate_of[id]];
        eprintln!(
            "  #{id} {}:{} {} ({:?})",
            files[candidate.file].path, candidate.start_line, candidate.name, candidate.owner
        );
        let mut calls: Vec<_> = function.calls.iter().collect();
        calls.sort_by_key(|(op, _)| **op);
        for (op, call) in calls {
            if let codegraph_analysis::ir::IrOp::Call { callee, .. } = &function.ir.body[*op] {
                eprintln!(
                    "    {callee} -> {:?}{}{}",
                    call.targets,
                    if call.in_project { " project" } else { "" },
                    if call.guessed { " guessed" } else { "" }
                );
            }
        }
    }
}

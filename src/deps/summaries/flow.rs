//! `codegraph deps flow`: a path query inside a project's dependencies.
//!
//! The function named in one dependency's shard and everything it calls —
//! through that shard's call edges and, where a call names another crate's
//! function by path (`matcher::Matcher::from_system`), into that crate's
//! shard (the one function of that name there) — lowered and solved like
//! the project's taint rules. Two questions:
//!
//! - **from the environment**: where `env::var` reads (in the function or
//!   anything it reaches) end up among its outputs — its return value
//!   (and which returned field), what it writes through `self` or a
//!   parameter — with every step, across crates;
//! - **from a parameter**: which calls' arguments the parameter's data
//!   reaches (and, as importantly, which it never reaches).
//!
//! Read-only (shards open immutable; only the project's dependency shards
//! are read, at most [`MAX_UNITS`]) and bounded by [`FlowLimits`]. A cut
//! bound makes the report `partial`: an absent path is then no proof.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use codegraph_analysis::ir::IrOp;
use codegraph_analysis::taint_flow::program::Solver;
use codegraph_analysis::taint_flow::{Budget, FuncId, Mark, Output, Step, TaintSpec};
use serde::Serialize;

use super::program::{Limits, Linking, Program, ShardFunctions, Unit};
use crate::deps::model::Ecosystem;
use crate::deps::shard::ShardHandle;
use crate::deps::store::DepsHome;
use crate::deps::{DepSource, dependencies_of_in};

/// Shards one query reads, at most.
pub const MAX_UNITS: usize = 32;

/// Where the query starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowFrom {
    Environment,
    /// A parameter, by name or position.
    Param(String),
}

/// Bounds on one query.
#[derive(Debug, Clone, Copy)]
pub struct FlowLimits {
    pub max_functions: usize,
    pub max_ops: usize,
    pub max_steps: usize,
    pub time: Duration,
}

impl Default for FlowLimits {
    fn default() -> Self {
        Self {
            max_functions: 20_000,
            max_ops: 2_000_000,
            max_steps: 20_000_000,
            time: Duration::from_secs(30),
        }
    }
}

/// One step of a reported path.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FlowStep {
    pub package: String,
    /// Absolute path in the dependency's source tree.
    pub file: String,
    pub line: u32,
    pub function: String,
    pub code: String,
}

/// One flow found.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FlowPath {
    /// What the data comes from (`std::env::var`, `param dst`).
    pub from: String,
    /// Where it arrives (`return`, `return.inner`, `self`, or a call's
    /// argument `Tunnel::new#0`).
    pub to: String,
    pub steps: Vec<FlowStep>,
}

/// What a query found.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowReport {
    pub package: String,
    pub function: String,
    /// Every function the name matched (`build` of several builders).
    pub matched: Vec<FlowStep>,
    pub from: String,
    pub flows: Vec<FlowPath>,
    /// Shards read.
    pub packages: Vec<String>,
    pub functions: usize,
    /// A bound cut the search: what is not reported may still flow.
    pub partial: bool,
    pub elapsed_ms: u64,
}

/// Run a query in the dependency `package` of the project at
/// `project_root`, on the function whose qualified name is `function` (or
/// ends with `::function`).
pub fn flow(
    home: &DepsHome,
    project_root: &Path,
    package: &str,
    function: &str,
    from: &FlowFrom,
    limits: &FlowLimits,
) -> Result<FlowReport, String> {
    let started = Instant::now();
    let deadline = started + limits.time;
    let available = project_shards(home, project_root)?;
    let target_handle = available
        .iter()
        .position(|h| same_package(&h.meta().name, package))
        .ok_or_else(|| format!("no built shard of `{package}` among the project's dependencies"))?;
    // The package's shard, then — round by round — the crates its code
    // (and theirs) names by path, at most MAX_UNITS in all.
    let mut open: Vec<usize> = vec![target_handle];
    let mut loaded: Vec<ShardFunctions> = Vec::new();
    let program_limits = Limits {
        max_functions: limits.max_functions,
        max_ops: limits.max_ops,
        deadline: Some(deadline),
    };
    let suffix = format!("::{function}");
    let seed_rows: Vec<usize>;
    let mut program;
    loop {
        while loaded.len() < open.len() {
            let handle = &available[open[loaded.len()]];
            loaded.push(ShardFunctions::load(handle, 200_000).map_err(|e| e.to_string())?);
        }
        let rows: Vec<usize> = loaded[0]
            .fns
            .iter()
            .enumerate()
            .filter(|(_, row)| row.qualified == function || row.qualified.ends_with(&suffix))
            .map(|(row, _)| row)
            .take(8)
            .collect();
        if rows.is_empty() {
            return Err(format!("`{package}` has no function `{function}`"));
        }
        let units: Vec<Unit<'_>> = open
            .iter()
            .zip(std::mem::take(&mut loaded))
            .map(|(&index, fns)| Unit {
                handle: &available[index],
                fns,
            })
            .collect();
        program = Program::new(units);
        let seeds: Vec<(usize, usize)> = rows.iter().map(|&row| (0, row)).collect();
        program.include(&seeds, Linking::ByName, &program_limits);
        let mut added = false;
        for krate in program.wanted_crates() {
            if open.len() >= MAX_UNITS {
                break;
            }
            if let Some(index) = available
                .iter()
                .position(|h| provides(h, &krate))
                .filter(|index| !open.contains(index))
            {
                open.push(index);
                added = true;
            }
        }
        let past = Instant::now() >= deadline;
        if !added || past {
            seed_rows = rows;
            break;
        }
        // Rebuild with the new shards (their functions reloaded).
        loaded = std::mem::take(&mut program.units)
            .into_iter()
            .map(|unit| unit.fns)
            .collect();
    }
    let seeds: Vec<(usize, usize)> = seed_rows.iter().map(|&row| (0, row)).collect();
    let packages: Vec<String> = open
        .iter()
        .map(|&i| available[i].key().to_string())
        .collect();
    if std::env::var_os("CODEGRAPH_TAINT_TRACE").is_some() {
        trace_program(&program);
    }
    let seed_ids: Vec<FuncId> = seeds
        .iter()
        .filter_map(|&(unit, row)| program.id_of(unit, row))
        .collect();
    let mut render = Render::default();
    let matched: Vec<FlowStep> = seed_ids
        .iter()
        .map(|&id| {
            let line = program.row(id).start_line;
            render.step(&program, id, line)
        })
        .collect();
    let mut budget = Budget::new(limits.max_steps, Some(deadline));
    let mut solver = Solver::new(&program.functions);
    let mut flows: Vec<FlowPath> = Vec::new();
    let partial;
    match from {
        FlowFrom::Environment => {
            let mut specs = program.specs();
            for spec in specs.values_mut() {
                spec.call_args.clear();
            }
            specs.retain(|_, spec| !spec.is_empty());
            let (solution, summaries) = solver.run_with_summaries(&specs, &mut budget);
            partial = solution.partial;
            if std::env::var_os("CODEGRAPH_TAINT_TRACE").is_some() {
                for (id, summary) in summaries.iter().enumerate() {
                    let Some(summary) = summary else {
                        eprintln!("  #{id} no summary");
                        continue;
                    };
                    let sources = summary.returns.sources.len()
                        + summary
                            .writes
                            .iter()
                            .map(|(_, o)| o.sources.len())
                            .sum::<usize>();
                    if sources > 0 {
                        eprintln!("  #{id} {} environment facts", sources);
                    }
                }
            }
            for &id in &seed_ids {
                let Some(summary) = summaries.get(id as usize).cloned().flatten() else {
                    continue;
                };
                let mut outputs: Vec<(String, &Output)> = vec![("return".into(), &summary.returns)];
                for (field, output) in &summary.returns_fields {
                    outputs.push((format!("return.{field}"), output));
                }
                for (target, output) in &summary.writes {
                    outputs.push((render.access(target), output));
                }
                for (to, output) in outputs {
                    for fact in &output.sources {
                        let Some((_, callee)) = program
                            .probes
                            .get(fact.source.func as usize)
                            .and_then(|p| p.environment.get(fact.source.index))
                        else {
                            continue;
                        };
                        flows.push(FlowPath {
                            from: callee.clone(),
                            to: to.clone(),
                            steps: render.steps(&program, &fact.path),
                        });
                    }
                }
            }
        }
        FlowFrom::Param(param) => {
            let mut specs: HashMap<FuncId, TaintSpec> = HashMap::new();
            // Every call's arguments are sinks.
            let mut names: HashMap<(FuncId, usize), String> = HashMap::new();
            for (id, function) in program.functions.iter().enumerate() {
                let spec = specs.entry(id as FuncId).or_default();
                for (op, ir_op) in function.ir.body.iter().enumerate() {
                    if let IrOp::Call { callee, args, .. } = ir_op {
                        if callee.starts_with('<') {
                            continue;
                        }
                        for arg in 0..args.len() {
                            names.insert(
                                (id as FuncId, spec.call_args.len()),
                                format!("{callee}#{arg}"),
                            );
                            spec.call_args.push((op, arg));
                        }
                    }
                }
            }
            for &id in &seed_ids {
                let ir = &program.functions[id as usize].ir;
                let index = ir
                    .params
                    .iter()
                    .position(|p| p.as_str() == param)
                    .or_else(|| param.parse::<usize>().ok().filter(|&i| i < ir.params.len()));
                let Some(index) = index else {
                    continue;
                };
                let span = ir.param_spans[index];
                specs.entry(id).or_default().sources.push(Mark {
                    start_byte: span.start_byte,
                    end_byte: span.end_byte,
                    kind: None,
                });
            }
            let solution = solver.run(&specs, &mut budget);
            partial = solution.partial;
            for flow in &solution.flows {
                let Some(to) = names.get(&(flow.sink.func, flow.sink.index)) else {
                    continue;
                };
                flows.push(FlowPath {
                    from: format!("param {param}"),
                    to: to.clone(),
                    steps: render.steps(&program, &flow.path),
                });
            }
        }
    }
    flows.sort_by(|a, b| (&a.to, a.steps.len()).cmp(&(&b.to, b.steps.len())));
    flows.dedup_by(|a, b| a.to == b.to && a.from == b.from);
    Ok(FlowReport {
        package: available[target_handle].key().to_string(),
        function: function.to_string(),
        matched,
        from: match from {
            FlowFrom::Environment => "environment".into(),
            FlowFrom::Param(p) => format!("param {p}"),
        },
        flows,
        packages,
        functions: program.functions.len(),
        partial: partial || program.partial || budget.is_exhausted(),
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

/// `hyper-util` and `hyper_util` name one crate.
fn same_package(name: &str, asked: &str) -> bool {
    name.replace('-', "_") == asked.replace('-', "_")
}

/// `CODEGRAPH_TAINT_TRACE=1`: the program on stderr — each function, its
/// crate, and what each call resolved to.
fn trace_program(program: &Program<'_>) {
    for (id, function) in program.functions.iter().enumerate() {
        let (unit, _) = program.origin[id];
        let row = program.row(id as FuncId);
        eprintln!(
            "  #{id} {} {}:{} {}",
            program.units[unit].handle.meta().name,
            row.file,
            row.start_line,
            row.qualified
        );
        let mut calls: Vec<_> = function.calls.iter().collect();
        calls.sort_by_key(|(op, _)| **op);
        for (op, call) in calls {
            if let IrOp::Call { callee, .. } = &function.ir.body[*op] {
                eprintln!("      {callee} -> {:?}", call.targets);
            }
        }
        let probes = &program.probes[id];
        for (_, callee) in &probes.environment {
            eprintln!("      env: {callee}");
        }
        if id == 0 && std::env::var("CODEGRAPH_TAINT_TRACE").is_ok_and(|v| v == "ir") {
            for (index, op) in function.ir.body.iter().enumerate() {
                eprintln!("        {index} L{} {op:?}", function.ir.span(index).line);
            }
        }
    }
}

/// Whether the shard is the crate code calls `krate`.
fn provides(handle: &ShardHandle, krate: &str) -> bool {
    let meta = handle.meta();
    if meta.ecosystem == Ecosystem::Rust {
        return crate::deps::toolchain::STD_CRATES.contains(&krate);
    }
    meta.name.replace('-', "_") == krate
}

/// The project's dependency shards (crates and the toolchain), opened
/// read-only only when their `meta.json` says there is one to read.
fn project_shards(home: &DepsHome, project_root: &Path) -> Result<Vec<ShardHandle>, String> {
    let deps = dependencies_of_in(home, project_root).map_err(|e| e.to_string())?;
    let mut handles: Vec<ShardHandle> = deps
        .iter()
        .filter(|d| matches!(d.key.ecosystem, Ecosystem::Crates | Ecosystem::Rust))
        .filter(|d| !matches!(d.source, DepSource::Path { .. }))
        .filter_map(|d| ShardHandle::open(home, &d.key))
        .collect();
    handles.dedup_by_key(|h| h.key());
    if handles.is_empty() {
        return Err(
            "no dependency shards recorded for this project (run `codegraph deps build`)".into(),
        );
    }
    Ok(handles)
}

/// Steps as a reader sees them: package, absolute file, line, code.
#[derive(Default)]
struct Render {
    lines: HashMap<PathBuf, Vec<String>>,
}

impl Render {
    fn step(&mut self, program: &Program<'_>, id: FuncId, line: u32) -> FlowStep {
        let (unit, _) = program.origin[id as usize];
        let handle = program.units[unit].handle;
        let row = program.row(id);
        let file = handle.source_dir().join(&row.file);
        let code = self.code(&file, line);
        FlowStep {
            package: handle.meta().name.clone(),
            file: file.to_string_lossy().into_owned(),
            line,
            function: row.qualified.clone(),
            code,
        }
    }

    fn steps(&mut self, program: &Program<'_>, path: &[Step]) -> Vec<FlowStep> {
        let mut out: Vec<FlowStep> = Vec::new();
        for step in path {
            if step.span.line == 0 || step.func as usize >= program.functions.len() {
                continue;
            }
            let rendered = self.step(program, step.func, step.span.line);
            if out.last().is_none_or(|last| {
                (last.file.as_str(), last.line) != (rendered.file.as_str(), rendered.line)
            }) {
                out.push(rendered);
            }
        }
        out
    }

    fn access(&self, access: &codegraph_analysis::taint_flow::Access) -> String {
        let slot = match &access.slot {
            codegraph_analysis::taint_flow::Slot::Receiver => "self".to_string(),
            codegraph_analysis::taint_flow::Slot::Param(i) => format!("p{i}"),
            codegraph_analysis::taint_flow::Slot::Global(k) => k.clone(),
        };
        std::iter::once(slot)
            .chain(access.fields.iter().cloned())
            .collect::<Vec<_>>()
            .join(".")
    }

    /// Line `line` of `file`, trimmed (files read once).
    fn code(&mut self, file: &Path, line: u32) -> String {
        let lines = self.lines.entry(file.to_path_buf()).or_insert_with(|| {
            std::fs::read_to_string(file)
                .map(|text| text.lines().map(str::to_string).collect())
                .unwrap_or_default()
        });
        lines
            .get(line.saturating_sub(1) as usize)
            .map(|l| l.trim().chars().take(120).collect())
            .unwrap_or_default()
    }
}

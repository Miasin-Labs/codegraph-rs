//! Taint summaries of a dependency's functions, computed once per crate
//! version (per shard) and read by every project whose taint rules call
//! into it.
//!
//! A summary says, over a function's inputs (its parameters, `self`, and
//! the fields below them): what its return value carries (and each
//! returned struct field, where narrower), what it writes that callers
//! see, which inputs reach an argument of a call the shard does not
//! resolve (another crate's or std's: `p0 → std::fs::read#0`), and where
//! the environment (`env::var`) reaches an output — every fact with its
//! path through the dependency's code. Summaries are the pure, rule-free
//! ones of [`codegraph_analysis::taint_flow::program`], over the shard's
//! functions lowered by the same Rust lowering the project's rules use and
//! joined by the shard's own call edges; a call into another crate is a
//! library call (its argument facts say where data goes).
//!
//! Built only by the CLI — `deps build` after a shard is built, and
//! `analyze rules` on first need — within a function, op, step and time
//! budget ([`BuildLimits`]); a budget cut makes the artifact `complete:
//! false`, never a failure. Readers (the taint rules' composition,
//! [`compose`]; MCP) only read ([`store::read`]).

pub mod compose;
pub mod flow;
pub mod program;
pub mod store;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use codegraph_analysis::taint_flow::program::Solver;
use codegraph_analysis::taint_flow::{Access, Budget, FuncId, Output, Slot, Step, Summary};

use self::program::{Limits, Linking, Program, ShardFunctions, Unit};
use self::store::{
    Artifact,
    FORMAT,
    SUMMARY_VERSION,
    ShardStamp,
    StoredAccess,
    StoredCallArg,
    StoredEnvironment,
    StoredFunction,
    StoredInput,
    StoredOutput,
    StoredStep,
    StoredSummary,
};
use super::model::DepKey;
use super::shard::ShardHandle;
use super::store::{DepsHome, StoreLock};

/// Bounds on one shard's summaries.
#[derive(Debug, Clone, Copy)]
pub struct BuildLimits {
    pub max_functions: usize,
    pub max_ops: usize,
    /// Search steps of the solver.
    pub max_steps: usize,
    pub time: Duration,
}

impl Default for BuildLimits {
    fn default() -> Self {
        Self {
            max_functions: 60_000,
            max_ops: 4_000_000,
            max_steps: 50_000_000,
            time: Duration::from_millis(
                env_ms("CODEGRAPH_DEP_SUMMARY_BUDGET_MS").unwrap_or(60_000),
            ),
        }
    }
}

fn env_ms(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.trim().parse().ok()
}

/// `CODEGRAPH_DEP_SUMMARIES=0` (or `false`/`off`) turns dependency
/// summaries off: neither built nor read.
pub fn summaries_enabled() -> bool {
    !std::env::var("CODEGRAPH_DEP_SUMMARIES").is_ok_and(|v| {
        let v = v.trim();
        v == "0" || v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("off")
    })
}

/// What [`ensure`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnsureOutcome {
    /// Built now: how many functions it summarized, in how long.
    Built {
        summaries: usize,
        complete: bool,
        elapsed_ms: u64,
    },
    /// A current artifact was already there.
    UpToDate,
    /// No readable shard for the key.
    NoShard,
    /// Another process holds the shard's lock (building it or its
    /// summaries).
    Locked,
    Failed(String),
}

/// Make sure the shard `key` has current summaries: build them (under the
/// shard's lock) when missing or stale. CLI only.
pub fn ensure(home: &DepsHome, key: &DepKey, limits: &BuildLimits) -> EnsureOutcome {
    let Some(handle) = ShardHandle::open(home, key) else {
        return EnsureOutcome::NoShard;
    };
    if store::read(handle.dir(), handle.meta()).is_some() {
        return EnsureOutcome::UpToDate;
    }
    let _lock = match StoreLock::try_acquire(&home.shard_lock_path(key)) {
        Ok(Some(lock)) => lock,
        Ok(None) => return EnsureOutcome::Locked,
        Err(error) => return EnsureOutcome::Failed(format!("lock: {error}")),
    };
    let artifact = match build_artifact(&handle, limits) {
        Ok(artifact) => artifact,
        Err(error) => return EnsureOutcome::Failed(error),
    };
    match store::write(handle.dir(), &artifact) {
        Ok(()) => EnsureOutcome::Built {
            summaries: artifact.summaries.len(),
            complete: artifact.complete,
            elapsed_ms: artifact.elapsed_ms,
        },
        Err(error) => EnsureOutcome::Failed(format!("write: {error}")),
    }
}

/// Summarize every Rust function of `handle`'s shard.
pub fn build_artifact(handle: &ShardHandle, limits: &BuildLimits) -> Result<Artifact, String> {
    let started = Instant::now();
    let deadline = started + limits.time;
    let fns = ShardFunctions::load(handle, limits.max_functions).map_err(|e| e.to_string())?;
    let total = fns.fns.len();
    let seeds: Vec<(usize, usize)> = (0..total)
        .filter(|&row| !program::is_test_path(&fns.fns[row].file))
        .map(|row| (0, row))
        .collect();
    let mut program = Program::new(vec![Unit { handle, fns }]);
    program.include(
        &seeds,
        Linking::None,
        &Limits {
            max_functions: limits.max_functions,
            max_ops: limits.max_ops,
            deadline: Some(deadline),
        },
    );
    let lowered_at = Instant::now();
    let specs = program.specs();
    let mut budget = Budget::new(limits.max_steps, Some(deadline));
    let mut solver = Solver::new(&program.functions);
    let (solution, summaries) = solver.run_with_summaries(&specs, &mut budget);
    if std::env::var_os("CODEGRAPH_TAINT_TRACE").is_some() {
        for (id, function) in program.functions.iter().enumerate() {
            if function.ir.body.len() > 5_000 {
                let row = program.row(id as FuncId);
                eprintln!(
                    "  large: {}:{} {} ({} ops)",
                    row.file,
                    row.start_line,
                    row.qualified,
                    function.ir.body.len()
                );
            }
        }
        eprintln!(
            "summaries {}: {} functions, {} ops lowered in {:?}, solved in {:?}",
            handle.key(),
            program.functions.len(),
            program.ops,
            lowered_at.duration_since(started),
            lowered_at.elapsed()
        );
    }
    let mut out = ArtifactWriter::new(&program);
    for (id, summary) in summaries.iter().enumerate() {
        if let Some(summary) = summary {
            out.add(id as FuncId, summary);
        }
    }
    Ok(Artifact {
        format: FORMAT.to_string(),
        version: SUMMARY_VERSION,
        shard: ShardStamp::of(handle.meta()),
        complete: !program.partial && !solution.partial && !budget.is_exhausted(),
        functions_total: total,
        functions_lowered: program.functions.len(),
        elapsed_ms: started.elapsed().as_millis() as u64,
        functions: out.functions,
        summaries: out.summaries,
    })
}

/// Stored steps of at most this many (first and last halves kept).
const MAX_STORED_PATH: usize = 16;

/// Summaries into their stored form, naming each function once.
struct ArtifactWriter<'p, 'a> {
    program: &'p Program<'a>,
    functions: Vec<StoredFunction>,
    index: HashMap<FuncId, u32>,
    summaries: Vec<StoredSummary>,
}

impl<'p, 'a> ArtifactWriter<'p, 'a> {
    fn new(program: &'p Program<'a>) -> Self {
        Self {
            program,
            functions: Vec::new(),
            index: HashMap::new(),
            summaries: Vec::new(),
        }
    }

    fn function(&mut self, id: FuncId) -> u32 {
        if let Some(&index) = self.index.get(&id) {
            return index;
        }
        let row = self.program.row(id);
        let ir = &self.program.functions[id as usize].ir;
        let index = self.functions.len() as u32;
        self.functions.push(StoredFunction {
            id: row.id.clone(),
            q: row.qualified.clone(),
            f: row.file.clone(),
            l: row.start_line,
            a: ir.params.len() as u32,
            r: ir.receiver.is_some(),
            u: self.program.lossy.get(id as usize).copied().unwrap_or(true),
        });
        self.index.insert(id, index);
        index
    }

    fn path(&mut self, steps: &[Step]) -> Vec<StoredStep> {
        let mut kept: Vec<&Step> = steps.iter().filter(|s| s.span.line > 0).collect();
        if kept.len() > MAX_STORED_PATH {
            let tail = kept.split_off(kept.len() - MAX_STORED_PATH / 2);
            kept.truncate(MAX_STORED_PATH / 2);
            kept.extend(tail);
        }
        let mut out: Vec<StoredStep> = Vec::with_capacity(kept.len());
        for step in kept {
            let stored = (self.function(step.func), step.span.line);
            if out.last() != Some(&stored) {
                out.push(stored);
            }
        }
        out
    }

    fn inputs(&mut self, output: &Output) -> Vec<StoredInput> {
        output
            .inputs
            .iter()
            .map(|(access, path)| StoredInput {
                input: stored_access(access),
                path: self.path(path),
            })
            .collect()
    }

    /// The environment facts of `output` (sources are the probes' env
    /// reads), stored as reaching `target`.
    fn environment(&mut self, output: &Output, target: &StoredAccess) -> Vec<StoredEnvironment> {
        output
            .sources
            .iter()
            .filter_map(|fact| {
                let (_, callee) = self
                    .program
                    .probes
                    .get(fact.source.func as usize)?
                    .environment
                    .get(fact.source.index)?;
                Some(StoredEnvironment {
                    output: target.clone(),
                    callee: callee.clone(),
                    path: self.path(&fact.path),
                })
            })
            .collect()
    }

    fn add(&mut self, id: FuncId, summary: &Summary) {
        let mut stored = StoredSummary {
            function: 0,
            returns: self.inputs(&summary.returns),
            ..StoredSummary::default()
        };
        let returned = StoredAccess {
            slot: "return".into(),
            fields: Vec::new(),
        };
        stored.environment = self.environment(&summary.returns, &returned);
        for (field, output) in &summary.returns_fields {
            let inputs = self.inputs(output);
            let target = StoredAccess {
                slot: "return".into(),
                fields: vec![field.clone()],
            };
            let environment = self.environment(output, &target);
            stored.environment.extend(environment);
            stored.fields.push((field.clone(), StoredOutput { inputs }));
        }
        for (target, output) in &summary.writes {
            let target = stored_access(target);
            let inputs = self.inputs(output);
            let environment = self.environment(output, &target);
            stored.environment.extend(environment);
            if !inputs.is_empty() {
                stored.writes.push((target, StoredOutput { inputs }));
            }
        }
        for (input, fact) in &summary.sinks {
            let Some((_, arg, callee)) = self
                .program
                .probes
                .get(fact.sink.func as usize)
                .and_then(|p| p.call_args.get(fact.sink.index))
            else {
                continue;
            };
            let (arg, callee) = (*arg as u32, callee.clone());
            let path = self.path(&fact.path);
            stored.calls.push(StoredCallArg {
                input: stored_access(input),
                callee,
                arg,
                path,
            });
        }
        // An empty summary is kept too: "hands back none of its inputs" is
        // what makes `constant(x)` clean where a library model is not.
        stored.function = self.function(id);
        self.summaries.push(stored);
    }
}

/// An input as stored: `self`, `p<i>`, `g:<key>`, and its fields.
fn stored_access(access: &Access) -> StoredAccess {
    StoredAccess {
        slot: match &access.slot {
            Slot::Receiver => "self".to_string(),
            Slot::Param(index) => format!("p{index}"),
            Slot::Global(key) => format!("g:{key}"),
        },
        fields: access.fields.clone(),
    }
}

/// A stored input as the analysis reads it (`None`: an output slot).
pub(crate) fn access_of(stored: &StoredAccess) -> Option<Access> {
    let slot = match stored.slot.as_str() {
        "self" => Slot::Receiver,
        other => match other.strip_prefix('p').and_then(|n| n.parse().ok()) {
            Some(index) => Slot::Param(index),
            None => Slot::Global(other.strip_prefix("g:")?.to_string()),
        },
    };
    Some(Access {
        slot,
        fields: stored.fields.clone(),
    })
}

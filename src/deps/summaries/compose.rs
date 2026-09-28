//! Dependency summaries as a project's taint program reads them: a call
//! the external pass resolved into a dependency shard (`external_edges`:
//! graph key + node id) takes that function's stored summary in place of
//! the library models, its paths' steps naming the dependency's own files
//! and lines ([`ExternalFunction`], program ids from
//! [`EXTERNAL_FUNC_BASE`]).
//!
//! Bounded: at most [`MAX_LOADED`] artifacts per run, none loaded past the
//! deadline. Read-only unless the caller allows building a missing
//! artifact ([`Access::BuildMissing`], the CLI's first need), and then
//! within its own time budget. A missing or stale artifact is a library
//! call, as before.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use codegraph_analysis::ir::Span;
use codegraph_analysis::taint_flow::{
    Access as AccessOf,
    FuncId,
    Output,
    Slot,
    Step,
    Summary,
    summary,
};

use super::store::{Artifact, StoredInput, StoredStep};
use super::{BuildLimits, EnsureOutcome, access_of, ensure, store};
use crate::deps::model::Ecosystem;
use crate::deps::shard::ShardMeta;
use crate::deps::store::DepsHome;

/// Program ids of dependency functions start here (the program's own
/// functions are numbered from 0).
pub const EXTERNAL_FUNC_BASE: FuncId = 1 << 30;
/// Artifacts loaded per run, at most.
pub const MAX_LOADED: usize = 32;

/// Whether a run may build what is missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Only read (MCP, the prompt hook, tests without a CLI).
    ReadOnly,
    /// Build a missing artifact on first need, within this much time in
    /// all (the CLI).
    BuildMissing(Duration),
}

/// A dependency function a path steps through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalFunction {
    /// Its file, absolute (in the dependency's source tree).
    pub file: PathBuf,
    pub qualified: String,
    /// The package (`reqwest`, `std`).
    pub package: String,
}

struct Loaded {
    artifact: Artifact,
    source_dir: PathBuf,
    package: String,
    /// Node id → index into `artifact.summaries`.
    by_node: HashMap<String, usize>,
}

/// What a run used, for traces and reports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ComposeStats {
    pub calls: usize,
    pub summarized: usize,
    pub artifacts_loaded: usize,
    pub artifacts_built: usize,
    pub artifacts_missing: usize,
}

/// The summaries a run reads, loaded on first need.
pub struct DependencySummaries {
    home: DepsHome,
    access: Access,
    deadline: Option<Instant>,
    loaded: HashMap<String, Option<Loaded>>,
    functions: Vec<ExternalFunction>,
    ids: HashMap<(String, u32), FuncId>,
    cache: HashMap<(String, String), Option<Arc<Summary>>>,
    build_left: Duration,
    pub stats: ComposeStats,
}

impl DependencySummaries {
    pub fn new(home: DepsHome, access: Access, deadline: Option<Instant>) -> Self {
        let build_left = match access {
            Access::BuildMissing(budget) => budget,
            Access::ReadOnly => Duration::ZERO,
        };
        Self {
            home,
            access,
            deadline,
            loaded: HashMap::new(),
            functions: Vec::new(),
            ids: HashMap::new(),
            cache: HashMap::new(),
            build_left,
            stats: ComposeStats::default(),
        }
    }

    /// The summary of node `node_id` of the dependency graph `graph_key`
    /// (`crates/reqwest-0.12.28`), when its shard has one.
    pub fn summary(&mut self, graph_key: &str, node_id: &str) -> Option<Arc<Summary>> {
        self.stats.calls += 1;
        let key = (graph_key.to_string(), node_id.to_string());
        if let Some(known) = self.cache.get(&key) {
            return known.clone();
        }
        let found = self.lookup(graph_key, node_id);
        if found.is_some() {
            self.stats.summarized += 1;
        }
        self.cache.insert(key, found.clone());
        found
    }

    /// The dependency function behind a program id from
    /// [`EXTERNAL_FUNC_BASE`].
    pub fn function(&self, id: FuncId) -> Option<&ExternalFunction> {
        self.functions
            .get(id.checked_sub(EXTERNAL_FUNC_BASE)? as usize)
    }

    fn lookup(&mut self, graph_key: &str, node_id: &str) -> Option<Arc<Summary>> {
        // std's bodies move data through raw pointers and intrinsics the
        // IR does not follow: its calls stay library calls (the tables).
        if graph_key.starts_with("rust/") {
            return None;
        }
        self.load(graph_key)?;
        let loaded = self.loaded.get(graph_key)?.as_ref()?;
        let index = *loaded.by_node.get(node_id)?;
        let stored = loaded.artifact.summaries[index].clone();
        let mut out = Summary {
            returns: self.output(graph_key, &stored.returns),
            ..Summary::default()
        };
        if out.returns.is_empty() {
            // A return that carries nothing from a body using `unsafe` may
            // be a flow the IR lost (raw pointers, intrinsics): then the
            // result carries its inputs, as a library call's does.
            if let Some(function) = self
                .loaded
                .get(graph_key)
                .and_then(|l| l.as_ref())
                .and_then(|l| l.artifact.functions.get(stored.function as usize))
                .filter(|function| function.u)
            {
                out.returns = default_returns(function);
            }
        }
        for (field, output) in &stored.fields {
            let output = self.output(graph_key, &output.inputs);
            out.returns_fields.push((field.clone(), output));
        }
        for (target, output) in &stored.writes {
            let Some(target) = access_of(target) else {
                continue;
            };
            let output = self.output(graph_key, &output.inputs);
            if !output.is_empty() {
                out.writes.push((target, output));
            }
        }
        Some(Arc::new(out))
    }

    fn output(&mut self, graph_key: &str, inputs: &[StoredInput]) -> Output {
        let mut out = Output::default();
        for input in inputs {
            let Some(access) = access_of(&input.input) else {
                continue;
            };
            let path = self.path(graph_key, &input.path);
            if !out.inputs.iter().any(|(a, _)| *a == access) {
                out.inputs.push((access, path));
            }
        }
        out
    }

    fn path(&mut self, graph_key: &str, steps: &[StoredStep]) -> summary::Path {
        let steps: Vec<Step> = steps
            .iter()
            .filter_map(|&(function, line)| {
                Some(Step {
                    func: self.function_id(graph_key, function)?,
                    span: Span {
                        line,
                        col: 0,
                        start_byte: 0,
                        end_byte: 0,
                    },
                    op: None,
                })
            })
            .collect();
        summary::path(steps)
    }

    fn function_id(&mut self, graph_key: &str, function: u32) -> Option<FuncId> {
        let key = (graph_key.to_string(), function);
        if let Some(&id) = self.ids.get(&key) {
            return Some(id);
        }
        let loaded = self.loaded.get(graph_key)?.as_ref()?;
        let stored = loaded.artifact.functions.get(function as usize)?;
        let id = EXTERNAL_FUNC_BASE + self.functions.len() as FuncId;
        self.functions.push(ExternalFunction {
            file: loaded.source_dir.join(&stored.f),
            qualified: stored.q.clone(),
            package: loaded.package.clone(),
        });
        self.ids.insert(key, id);
        Some(id)
    }

    /// Load (or, allowed, build then load) the artifact of `graph_key`.
    fn load(&mut self, graph_key: &str) -> Option<()> {
        if let Some(known) = self.loaded.get(graph_key) {
            return known.as_ref().map(|_| ());
        }
        let past_deadline = self.deadline.is_some_and(|d| Instant::now() >= d);
        let full = self.loaded.values().filter(|l| l.is_some()).count() >= MAX_LOADED;
        let loaded = if past_deadline || full {
            None
        } else {
            self.read_or_build(graph_key)
        };
        let found = loaded.is_some();
        if found {
            self.stats.artifacts_loaded += 1;
        } else {
            self.stats.artifacts_missing += 1;
        }
        self.loaded.insert(graph_key.to_string(), loaded);
        found.then_some(())
    }

    fn read_or_build(&mut self, graph_key: &str) -> Option<Loaded> {
        let (ecosystem, name) = shard_dir_of(graph_key)?;
        let dir = self.home.ecosystem_dir(ecosystem).join(name);
        let meta =
            ShardMeta::read(&dir).filter(|m| m.key().dir_name() == name && m.is_readable())?;
        let key = meta.key();
        let artifact = match store::read(&dir, &meta) {
            Some(artifact) => artifact,
            None if matches!(self.access, Access::BuildMissing(_))
                && !self.build_left.is_zero() =>
            {
                let started = Instant::now();
                let mut limits = BuildLimits::default();
                limits.time = limits.time.min(self.build_left);
                let outcome = ensure(&self.home, &key, &limits);
                self.build_left = self.build_left.saturating_sub(started.elapsed());
                if matches!(outcome, EnsureOutcome::Built { .. }) {
                    self.stats.artifacts_built += 1;
                }
                store::read(&dir, &meta)?
            }
            None => return None,
        };
        let by_node = artifact
            .summaries
            .iter()
            .enumerate()
            .filter_map(|(index, s)| {
                let function = artifact.functions.get(s.function as usize)?;
                Some((function.id.clone(), index))
            })
            .collect();
        Some(Loaded {
            source_dir: PathBuf::from(&meta.source_dir),
            package: meta.name.clone(),
            artifact,
            by_node,
        })
    }
}

/// Every parameter, and the receiver, as what a return carries (paths
/// empty: the call itself is the step).
fn default_returns(function: &super::store::StoredFunction) -> Output {
    let mut out = Output::default();
    let slots = function
        .r
        .then_some(Slot::Receiver)
        .into_iter()
        .chain((0..function.a as usize).map(Slot::Param));
    for slot in slots {
        out.inputs
            .push((AccessOf::slot(slot), summary::path(Vec::new())));
    }
    out
}

/// `crates/reqwest-0.12.28` → its ecosystem and shard directory name:
/// exactly one directory below an ecosystem (a linked project's root is
/// no shard). The shard's own `meta.json` then names its key.
fn shard_dir_of(graph_key: &str) -> Option<(Ecosystem, &str)> {
    let (ecosystem, dir) = graph_key.split_once('/')?;
    if dir.is_empty() || dir.contains(['/', '\\']) || dir.starts_with('.') {
        return None;
    }
    Some((ecosystem.parse().ok()?, dir))
}

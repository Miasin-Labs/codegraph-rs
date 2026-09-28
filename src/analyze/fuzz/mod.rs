//! Fuzzing as confirmation: the graph picks fuzz targets and writes their
//! harnesses, so a crash turns a static finding into a proven bug.
//!
//! - [`fuzz_targets`] (`codegraph analyze fuzz-targets`) ranks the functions
//!   an outside crate can call by what a fuzzer can feed them (bytes, text,
//!   a reader, `Arbitrary` values — [`model::InputShape`]) and why they are
//!   worth it: a parser-like name, risk sites (`unsafe`, indexing, panics,
//!   overflowing arithmetic, loops) in the body and — discounted per hop —
//!   in everything it calls, recursion, fan-in, and bug findings
//!   (`analyze bugs` + built-in rules) inside. Functions an existing fuzz
//!   target already calls are left out; ones it reaches are discounted.
//!   `--finding file:line` / `--function` rank only the targets that reach
//!   that place, nearest first.
//! - [`fuzz_harness`] writes a cargo-fuzz project for one target.
//! - [`run`] (`analyze fuzz-run`) runs it, bounded, and maps a crash back
//!   to the indexed function.
//!
//! Everything comes from the index — functions, signatures, return types,
//! visibility, resolved calls ([`crate::analyze::bugs::Project`]'s bulk
//! load) — plus one tree-sitter walk per file for what the index does not
//! keep ([`sites`]: risk sites, generic bounds, `impl Trait for`, the
//! exact visibility).
//!
//! **Ecosystems.** The ranking, graph features and report are language
//! neutral ([`model`], [`graph`], [`sites`] with its per-language table).
//! An ecosystem supplies the rest: which functions the package exposes and
//! by what path, how each parameter type maps to an [`model::InputShape`],
//! the existing targets, the harness project, and the runner. Rust
//! (cargo-fuzz) is [`rust`]; another (Go native fuzzing, Jazzer, Atheris)
//! adds a sibling module and a [`sites::SiteRules`] entry.

pub mod graph;
pub mod model;
pub mod run;
pub mod rust;
pub mod sites;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use graph::CallGraph;
use model::{Features, Harnessable, InputShape, ParamInfo, ReceiverInfo, SiteCounts};
use rust::api::RustApi;
use rust::existing::ExistingTargets;
use rust::harness::{self, WrittenFile};
use rust::{RustCallable, RustContext};
use serde::Serialize;

use super::bugs::{self, BugsOptions, Project};
use crate::codegraph::CodeGraph;

/// A place to aim at: a finding's `file:line`, or a function.
#[derive(Debug, Clone)]
pub enum Focus {
    Finding { file: String, line: u32 },
    Function(String),
}

/// What [`fuzz_targets`] ranks.
#[derive(Debug, Clone)]
pub struct TargetsOptions {
    pub focus: Option<Focus>,
    pub top: usize,
    /// Count `analyze bugs` and built-in rule findings as a feature.
    pub findings: bool,
}

impl Default for TargetsOptions {
    fn default() -> Self {
        Self {
            focus: None,
            top: 20,
            findings: true,
        }
    }
}

/// One ranked target.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FuzzTarget {
    pub rank: usize,
    pub name: String,
    pub qualified_name: String,
    pub file: String,
    pub line: u32,
    /// How a fuzz target calls it (`blurhash::decode`).
    pub path: String,
    /// The cargo-fuzz target name `fuzz-harness` gives it.
    pub target: String,
    pub score: f64,
    /// The richest input it takes.
    pub input: InputShape,
    pub params: Vec<ParamInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver: Option<ReceiverInfo>,
    pub harness: Harnessable,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub todo: Vec<String>,
    pub features: Features,
    /// The riskiest functions it reaches (most risk first).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reaches: Vec<String>,
    /// Calls from this target to the focus (`--finding`/`--function`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance: Option<u32>,
}

/// The focus as resolved.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FocusReport {
    /// The functions aimed at (qualified names).
    pub functions: Vec<String>,
    /// Exposed callables that reach them.
    pub reached_by: usize,
}

/// Result of [`fuzz_targets`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetsReport {
    pub ecosystem: &'static str,
    /// Library crates found (package names).
    pub crates: Vec<String>,
    /// Functions in the index.
    pub functions: usize,
    /// Functions an outside crate can call.
    pub callable: usize,
    pub targets: Vec<FuzzTarget>,
    /// Ranked targets left out by `top`.
    pub omitted: usize,
    /// Existing cargo-fuzz targets.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub existing_targets: Vec<String>,
    /// Callables left out because an existing target calls them.
    pub already_fuzzed: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus: Option<FocusReport>,
    pub note: String,
}

/// Rank the functions worth fuzzing in the project indexed at `root`.
pub fn fuzz_targets(
    cg: &CodeGraph,
    root: &Path,
    options: &TargetsOptions,
) -> Result<TargetsReport, String> {
    let analysis = Analysis::load(cg, root, options.findings)?;
    analysis.rank(options)
}

/// Loaded once, shared by ranking and harness generation.
pub(crate) struct Analysis {
    pub(crate) project: Project,
    pub(crate) graph: CallGraph,
    pub(crate) syntax: HashMap<String, sites::FnSyntax>,
    pub(crate) api: RustApi,
    existing: ExistingTargets,
    /// Findings per function (dense index).
    findings: Vec<usize>,
}

impl Analysis {
    pub(crate) fn load(cg: &CodeGraph, root: &Path, with_findings: bool) -> Result<Self, String> {
        let mut project = Project::load(cg, root)?;
        let mut graph = CallGraph::new(&project);
        let rust_files: Vec<String> = project
            .files()
            .iter()
            .filter(|file| file.ends_with(".rs"))
            .cloned()
            .collect();
        let syntax = sites::collect(&mut project, &rust_files);
        let api = RustApi::load(cg.query_builder().db().conn(), root, project.files())?;
        let existing = ExistingTargets::find(&project, &api, root);
        // A function returning a project iterator does its work in that
        // iterator's `next`, which the harness drains: count it as a call.
        let lazy: Vec<(usize, usize)> = {
            let context = RustContext::new(&api, &graph.functions, &syntax);
            graph
                .functions
                .iter()
                .enumerate()
                .filter_map(|(i, span)| {
                    let returns = rust::types::return_type(span.signature.as_deref()?)?;
                    context.lazy_next(&returns).map(|(next, _)| (i, next))
                })
                .filter(|(i, next)| i != next)
                .collect()
        };
        graph.add_edges(&lazy);

        let mut findings = vec![0usize; graph.functions.len()];
        if with_findings {
            let options = BugsOptions::default();
            let mut all = bugs::detect(&mut project, &options);
            let rules = crate::analyze::rules::RuleSet::load(&[], &[], true);
            if let Ok(found) = crate::analyze::rules::detect(cg, &mut project, &rules, &options) {
                all.extend(found);
            }
            for finding in all {
                let index = project
                    .enclosing_function(&finding.file, finding.line)
                    .and_then(|span| graph.index_of(&span.id));
                if let Some(index) = index {
                    findings[index] += 1;
                }
            }
        }
        Ok(Self {
            project,
            graph,
            syntax,
            api,
            existing,
            findings,
        })
    }

    pub(crate) fn context(&self) -> RustContext<'_> {
        RustContext::new(&self.api, &self.graph.functions, &self.syntax)
    }

    pub(crate) fn sites(&self, index: usize) -> SiteCounts {
        self.syntax
            .get(&self.graph.functions[index].id)
            .map(|syntax| syntax.sites)
            .unwrap_or_default()
    }

    /// The functions `focus` names (dense indices).
    pub(crate) fn resolve_focus(&self, focus: &Focus) -> Result<Vec<usize>, String> {
        match focus {
            Focus::Finding { file, line } => {
                let span = self
                    .project
                    .enclosing_function(file, *line)
                    .ok_or_else(|| format!("no indexed function contains {file}:{line}"))?;
                Ok(self.graph.index_of(&span.id).into_iter().collect())
            }
            Focus::Function(name) => {
                let tail = self
                    .api
                    .crates()
                    .iter()
                    .find_map(|krate| name.strip_prefix(&format!("{}::", krate.lib_name)))
                    .unwrap_or(name);
                let exact: Vec<usize> = (0..self.graph.functions.len())
                    .filter(|&i| {
                        let span = &self.graph.functions[i];
                        // `Type::m`, or a public path ending in it
                        // (`string::BitString::m` for `BitString::m`).
                        !span.is_test
                            && (span.qualified_name == *name
                                || span.qualified_name == tail
                                || tail.ends_with(&format!("::{}", span.qualified_name)))
                    })
                    .collect();
                if !exact.is_empty() {
                    return Ok(exact);
                }
                let loose: Vec<usize> = (0..self.graph.functions.len())
                    .filter(|&i| {
                        let span = &self.graph.functions[i];
                        !span.is_test
                            && (span.name == tail
                                || span.qualified_name.ends_with(&format!("::{tail}")))
                    })
                    .collect();
                if !loose.is_empty() {
                    return Ok(loose);
                }
                // A public path through a re-export or alias
                // (`multihash::Multihash::from_bytes` for
                // `MultihashGeneric::from_bytes`), as `fuzz-targets` prints.
                let context = self.context();
                let by_path: Vec<usize> = (0..self.graph.functions.len())
                    .filter(|&i| {
                        let span = &self.graph.functions[i];
                        span.name == name.rsplit("::").next().unwrap_or(name)
                            && context
                                .callable(span)
                                .is_some_and(|callable| callable.display_path() == *name)
                    })
                    .collect();
                if by_path.is_empty() {
                    Err(format!("no indexed function named `{name}`"))
                } else {
                    Ok(by_path)
                }
            }
        }
    }

    fn features(&self, index: usize, covered: &HashMap<usize, String>) -> (Features, Vec<String>) {
        let span = &self.graph.functions[index];
        let reach = self.graph.reach(index, 4, 400);
        let mut reach_risk = 0.0;
        let mut recursive = false;
        let mut findings = 0;
        let mut risky: Vec<(f64, &str)> = Vec::new();
        for &(other, depth) in &reach {
            let risk = self.sites(other).risk();
            reach_risk += risk * 0.6f64.powi(depth as i32);
            recursive |= self.graph.recursive[other];
            findings += self.findings[other];
            if depth > 0 && risk > 0.0 {
                risky.push((risk, &self.graph.functions[other].qualified_name));
            }
        }
        risky.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(b.1)));
        let reaches = risky
            .iter()
            .take(5)
            .map(|(_, name)| name.to_string())
            .collect();
        let fan_in = self
            .graph
            .callers(index)
            .iter()
            .filter(|&&caller| !self.graph.functions[caller].is_test)
            .count();
        let features = Features {
            parser_name: model::is_parser_name(&span.name),
            own: self.sites(index),
            reach_risk: (reach_risk * 100.0).round() / 100.0,
            reachable: reach.len(),
            fan_in,
            recursive,
            findings,
            covered_by: covered.get(&index).cloned(),
        };
        (features, reaches)
    }

    /// Functions existing targets call (direct) and reach (discounted).
    fn coverage(
        &self,
        callables: &[(usize, RustCallable)],
    ) -> (HashSet<usize>, HashMap<usize, String>) {
        let mut direct = HashSet::new();
        let mut reached: HashMap<usize, String> = HashMap::new();
        if self.existing.targets.is_empty() {
            return (direct, reached);
        }
        let mut roots: Vec<(usize, String)> = Vec::new();
        for (index, span) in self.graph.functions.iter().enumerate() {
            if let Some(target) = self.existing.called.get(&span.id) {
                roots.push((index, target.clone()));
            }
        }
        for (index, callable) in callables {
            if let Some(target) = self
                .existing
                .caller_of(&callable.span.id, &callable.span.name)
            {
                roots.push((*index, target.to_string()));
            }
        }
        for (root, target) in roots {
            direct.insert(root);
            for (other, _) in self.graph.reach(root, 8, 5000) {
                reached.entry(other).or_insert_with(|| target.clone());
            }
        }
        (direct, reached)
    }

    fn rank(&self, options: &TargetsOptions) -> Result<TargetsReport, String> {
        let context = self.context();
        let callables: Vec<(usize, RustCallable)> = (0..self.graph.functions.len())
            .filter_map(|i| context.callable(&self.graph.functions[i]).map(|c| (i, c)))
            .collect();
        let (direct, reached) = self.coverage(&callables);

        let (focus_distance, focus_report) = match &options.focus {
            Some(focus) => {
                let aimed = self.resolve_focus(focus)?;
                let mut distance: HashMap<usize, u32> = HashMap::new();
                for &function in &aimed {
                    for (caller, depth) in self.graph.reverse_reach(function, 8, 5000) {
                        let entry = distance.entry(caller).or_insert(depth);
                        *entry = (*entry).min(depth);
                    }
                }
                let report = FocusReport {
                    functions: aimed
                        .iter()
                        .map(|&i| self.graph.functions[i].qualified_name.clone())
                        .collect(),
                    reached_by: callables
                        .iter()
                        .filter(|(i, _)| distance.contains_key(i))
                        .count(),
                };
                (Some(distance), Some(report))
            }
            None => (None, None),
        };

        let mut already_fuzzed = 0;
        let mut targets: Vec<FuzzTarget> = Vec::new();
        for (index, callable) in &callables {
            let distance = match &focus_distance {
                Some(distances) => match distances.get(index) {
                    Some(&d) => Some(d),
                    None => continue,
                },
                None => None,
            };
            if direct.contains(index) && focus_distance.is_none() {
                already_fuzzed += 1;
                continue;
            }
            let (features, reaches) = self.features(*index, &reached);
            let params = callable.param_infos();
            let mut score = model::score(
                model::input_weight(&params),
                callable.receiver_weight(),
                &features,
            );
            if let Some(d) = distance {
                score *= 0.75f64.powi(d as i32);
            }
            let input = params
                .iter()
                .map(|p| p.shape)
                .min()
                .unwrap_or(InputShape::Fixed);
            let span = &callable.span;
            targets.push(FuzzTarget {
                rank: 0,
                name: span.name.clone(),
                qualified_name: span.qualified_name.clone(),
                file: span.file.clone(),
                line: span.start_line,
                path: callable.display_path(),
                target: harness::target_name(callable),
                score: (score * 1000.0).round() / 1000.0,
                input,
                params,
                receiver: callable.receiver_info(),
                harness: callable.harnessable(),
                todo: callable.todos.clone(),
                features,
                reaches,
                distance,
            });
        }
        targets.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| (&a.file, a.line).cmp(&(&b.file, b.line)))
        });
        for (rank, target) in targets.iter_mut().enumerate() {
            target.rank = rank + 1;
        }
        let top = options.top.max(1);
        let omitted = targets.len().saturating_sub(top);
        targets.truncate(top);
        Ok(TargetsReport {
            ecosystem: "rust",
            crates: self.api.crates().iter().map(|k| k.package.clone()).collect(),
            functions: self.graph.functions.len(),
            callable: callables.len(),
            targets,
            omitted,
            existing_targets: self.existing.targets.clone(),
            already_fuzzed,
            focus: focus_report,
            note: "Scores multiply what a fuzzer can feed (bytes and text first; a method needs a \
                   constructor) by the reasons to feed it: a parser-like name, risk sites in it and \
                   what it calls, recursion, fan-in and bug findings inside. Generate a harness with \
                   `codegraph analyze fuzz-harness --function <path>`; a crash confirms the bug."
                .to_string(),
        })
    }
}

/// What [`fuzz_harness`] generates.
#[derive(Debug, Clone, Default)]
pub struct HarnessOptions {
    /// A function (qualified name or public path); when it is not callable
    /// from outside, the best target reaching it is used.
    pub function: Option<String>,
    /// Or: the best target reaching this `file:line`.
    pub finding: Option<(String, u32)>,
    /// The cargo-fuzz directory (default `<crate>/fuzz`).
    pub out: Option<PathBuf>,
    /// Overwrite an existing target file with other content.
    pub force: bool,
    /// Render only; write nothing.
    pub dry_run: bool,
}

/// Result of [`fuzz_harness`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessReport {
    /// The function the harness calls.
    pub function: String,
    pub path: String,
    pub file: String,
    pub line: u32,
    /// Set when the asked-for function was not callable and this target
    /// reaches it instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reaches: Option<String>,
    pub target: String,
    pub fuzz_dir: String,
    pub harness: Harnessable,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub todo: Vec<String>,
    pub files: Vec<WrittenFile>,
    pub source: String,
    /// How to run it.
    pub command: String,
}

/// Generate (and write) a cargo-fuzz harness.
pub fn fuzz_harness(
    cg: &CodeGraph,
    root: &Path,
    options: &HarnessOptions,
) -> Result<HarnessReport, String> {
    let analysis = Analysis::load(cg, root, false)?;
    let context = analysis.context();
    let focus = match (&options.function, &options.finding) {
        (Some(function), _) => Focus::Function(function.clone()),
        (None, Some((file, line))) => Focus::Finding {
            file: file.clone(),
            line: *line,
        },
        (None, None) => return Err("name a function (--function) or a finding (--finding)".into()),
    };
    // The function itself when callable, else the best target reaching it.
    let aimed = analysis.resolve_focus(&focus)?;
    let direct = options.function.as_ref().and_then(|_| {
        aimed
            .iter()
            .find_map(|&i| context.callable(&analysis.graph.functions[i]))
    });
    let (callable, reaches) = match direct {
        Some(callable) => (callable, None),
        None => {
            let ranked = analysis.rank(&TargetsOptions {
                focus: Some(focus),
                top: 1,
                findings: false,
            })?;
            let best = ranked.targets.first().ok_or_else(|| {
                format!(
                    "no public function reaches {} — nothing a fuzz target can call",
                    ranked
                        .focus
                        .map(|f| f.functions.join(", "))
                        .unwrap_or_default()
                )
            })?;
            let span = analysis
                .graph
                .functions
                .iter()
                .find(|span| {
                    span.file == best.file && span.start_line == best.line && span.name == best.name
                })
                .ok_or("ranked target vanished")?;
            let callable = context
                .callable(span)
                .ok_or("ranked target is not callable")?;
            let aimed_names: Vec<String> = aimed
                .iter()
                .map(|&i| analysis.graph.functions[i].qualified_name.clone())
                .collect();
            (callable, Some(aimed_names.join(", ")))
        }
    };
    let text = harness::render(&callable);
    let dir = harness::fuzz_dir(root, &callable, options.out.as_deref());
    let files = if options.dry_run {
        Vec::new()
    } else {
        harness::write(root, &callable, &text, &dir, options.force)?
    };
    Ok(HarnessReport {
        function: callable.span.qualified_name.clone(),
        path: callable.display_path(),
        file: callable.span.file.clone(),
        line: callable.span.start_line,
        reaches,
        command: format!(
            "codegraph analyze fuzz-run --target {} --seconds 60{}",
            text.target,
            if options.out.is_some() {
                format!(" --fuzz-dir {}", dir.display())
            } else {
                String::new()
            }
        ),
        target: text.target,
        fuzz_dir: dir.to_string_lossy().to_string(),
        harness: callable.harnessable(),
        todo: callable.todos.clone(),
        files,
        source: text.source,
    })
}

/// The indexed function at a crash frame (`file` absolute, or relative to
/// the project or a crate): the first frame that lands in project code.
pub fn locate_crash(
    cg: &CodeGraph,
    root: &Path,
    frames: &[run::Frame],
) -> Result<Option<CrashSite>, String> {
    let project = Project::load(cg, root)?;
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    for frame in frames {
        let Some(file) = project_file(&project, root, &canonical, &frame.file) else {
            continue;
        };
        if file.contains("fuzz_targets/") {
            continue;
        }
        let function = project
            .enclosing_function(&file, frame.line)
            .map(|span| span.qualified_name.clone());
        return Ok(Some(CrashSite {
            file,
            line: frame.line,
            function,
        }));
    }
    Ok(None)
}

/// Where a crash happened in project code.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CrashSite {
    pub file: String,
    pub line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
}

pub(crate) fn project_file(
    project: &Project,
    root: &Path,
    canonical: &Path,
    path: &str,
) -> Option<String> {
    let as_path = Path::new(path);
    for base in [canonical, root] {
        if let Ok(relative) = as_path.strip_prefix(base) {
            let relative = relative.to_string_lossy().to_string();
            if project.files().contains(&relative) {
                return Some(relative);
            }
        }
    }
    // Relative to some crate (or the fuzz dir's `..`): the unique indexed
    // file with that suffix.
    let trimmed = path.trim_start_matches("./").trim_start_matches("../");
    let mut matches = project
        .files()
        .iter()
        .filter(|f| *f == trimmed || f.ends_with(&format!("/{trimmed}")));
    let first = matches.next()?;
    matches.next().is_none().then(|| first.clone())
}

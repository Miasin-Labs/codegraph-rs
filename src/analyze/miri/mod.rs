//! Miri as the undefined-behaviour oracle: the graph chooses what to run,
//! the real Miri runs it, and its report is mapped back onto the graph.
//!
//! `codegraph analyze miri [--finding F:L | --function Q | --all-unsafe]`:
//!
//! 1. **Aim** at functions: a finding's enclosing function, a named one,
//!    or (the default) every function with an `unsafe` block or `unsafe`
//!    modifier ([`crate::analyze::fuzz`]'s site walk).
//! 2. **Choose tests** ([`select`]): the project's `#[test]`s that reach an
//!    aimed function through the index's call edges, nearest first. Where
//!    none does, **generate a harness** ([`harness`]): the function — or the
//!    nearest public function reaching it — called the way `fuzz-harness`
//!    calls it, on fixed edge-case inputs, one test per input.
//! 3. **Run** ([`run`]) `cargo +nightly miri test` per test binary (built
//!    once) and per test, each under a wall-clock deadline that kills the
//!    process group, with a capped log and the `MIRIFLAGS` asked for
//!    (strict provenance by default; stacked or tree borrows; isolation on
//!    unless disabled).
//! 4. **Read** Miri's diagnostic ([`diagnostic`]): the UB kind, the primary
//!    span, the backtrace; **map** its project frames to indexed functions,
//!    and report each UB (or leak) as a [`Finding`] (`Detector::Miri`, rule
//!    `miri::<kind>`, confidence 1.0 — Miri reporting UB is proof for that
//!    execution) with the frames as evidence. Code Miri cannot run (FFI,
//!    inline assembly, an unsupported or isolated operation) is reported as
//!    `unsupported` with Miri's reason, never as clean; a clean run proves
//!    only that execution.
//! 5. **Confirm** static findings (rules, deviance, lint, any detector that
//!    reports a [`Finding`]) inside a function Miri proved UB in, and save
//!    the Miri findings (`.codegraph/miri/report.json`) so `analyze
//!    review` shows them beside the static findings they confirm.
//!
//! Heavy work: only the CLI runs this, never MCP or the prompt hook.

pub mod diagnostic;
pub mod harness;
pub mod run;
pub mod select;

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use diagnostic::{MiriDiagnostic, Place, parse_output};
use run::{Binary, Invocation, MiriOptions, RunStatus};
use select::{Selected, TestTarget};
use serde::{Deserialize, Serialize};

use super::bugs::{self, BugsOptions, Detector, Evidence, Finding, Project};
use super::fuzz::{Analysis, Focus, project_file};
use crate::codegraph::CodeGraph;

/// What to aim at.
#[derive(Debug, Clone)]
pub enum MiriFocus {
    /// The function containing `file:line`.
    Finding { file: String, line: u32 },
    /// A function by qualified name or public path.
    Function(String),
    /// Every function with `unsafe` in it.
    AllUnsafe,
    /// Several findings and functions (repeated `--finding`/`--function`):
    /// their union. One that names no indexed function is reported as
    /// uncovered unless none resolves.
    Several(Vec<MiriFocus>),
}

/// A `codegraph analyze miri` request.
#[derive(Debug, Clone)]
pub struct MiriRequest {
    pub focus: MiriFocus,
    /// At most this many test runs (existing tests and harness inputs).
    pub max_tests: usize,
    /// Generate harnesses for aimed functions no test reaches.
    pub harness: bool,
    /// Plan only: choose tests and harnesses, run nothing, write nothing.
    pub dry_run: bool,
    pub options: MiriOptions,
}

impl Default for MiriRequest {
    fn default() -> Self {
        Self {
            focus: MiriFocus::AllUnsafe,
            max_tests: 16,
            harness: true,
            dry_run: false,
            options: MiriOptions::default(),
        }
    }
}

/// An aimed function.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Aimed {
    pub function: String,
    pub file: String,
    pub line: u32,
}

/// Where a run comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunKind {
    /// A test of the project's own.
    Test,
    /// An input of a generated harness.
    Harness,
    /// A Miri log read with `--log`.
    Log,
}

/// One planned test run.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedRun {
    pub kind: RunKind,
    pub package: String,
    pub target: TestTarget,
    /// The libtest path (`tests::roundtrip`) or harness input (`empty`).
    pub test: String,
    /// The test's (or harnessed function's) place.
    pub file: String,
    pub line: u32,
    /// The harnessed function (public path), for a harness run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calls: Option<String>,
    /// Aimed functions it reaches.
    pub reaches: Vec<String>,
    pub distance: u32,
    #[serde(skip)]
    manifest: PathBuf,
}

/// A generated harness.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessPlan {
    /// The aimed function it is for.
    pub function: String,
    /// What it calls (the function, or the nearest public one reaching it).
    pub calls: String,
    pub target: String,
    pub dir: String,
    pub cases: Vec<String>,
    /// The generated `tests/<target>.rs`.
    pub source: String,
}

/// An aimed function nothing runs, and why.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Uncovered {
    pub function: String,
    pub file: String,
    pub line: u32,
    pub reason: String,
}

/// A project place in Miri's report, mapped to the index.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Site {
    pub file: String,
    pub line: u32,
    pub col: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    /// Miri's name for the frame (`tests::t_uaf`), for a backtrace frame.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame: Option<String>,
    #[serde(skip)]
    function_id: Option<String>,
}

/// One executed run.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunReport {
    #[serde(flatten)]
    pub planned: PlannedRun,
    #[serde(flatten)]
    pub invocation: Invocation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<MiriDiagnostic>,
    /// Where it happened in project code (the primary span when it is
    /// project code, else the innermost project frame).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site: Option<Site>,
    /// The backtrace's project frames, innermost first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<Site>,
}

/// A static finding in a function Miri proved UB in.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Confirmed {
    pub finding: Finding,
    /// The Miri rule (`miri::use-after-free`) and where.
    pub by: String,
    pub at: String,
    pub test: String,
}

/// Counts by outcome.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub runs: usize,
    pub ub: usize,
    pub leaks: usize,
    pub clean: usize,
    pub test_failed: usize,
    /// Unsupported, build failures, timeouts, ignored…: nothing proven.
    pub inconclusive: usize,
    pub by_status: BTreeMap<String, usize>,
}

/// Result of [`miri_report`].
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MiriReport {
    pub miriflags: String,
    pub dry_run: bool,
    pub aimed: Vec<Aimed>,
    /// `#[test]` functions found in the project.
    pub tests_found: usize,
    pub planned: Vec<PlannedRun>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub harnesses: Vec<HarnessPlan>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<RunReport>,
    /// UB and leaks, as findings.
    pub findings: Vec<Finding>,
    /// Static findings in functions Miri proved UB in.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub confirmed: Vec<Confirmed>,
    /// Aimed functions nothing ran.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub uncovered: Vec<Uncovered>,
    pub summary: Summary,
    pub note: String,
}

const NOTE: &str = "Miri interprets the test and reports the first Undefined Behavior on that \
    execution: a `ub` run is proof, a `clean` run proves only the inputs that test used, and \
    `unsupported`/`build-failed`/`timeout` runs prove nothing either way. Aimed functions no \
    test or harness reached are listed as uncovered, never as clean.";

/// Plan (and, unless `dry_run`, run) Miri over the project indexed at `root`.
pub fn miri_report(
    cg: &CodeGraph,
    root: &Path,
    request: &MiriRequest,
) -> Result<MiriReport, String> {
    let mut analysis = Analysis::load(cg, root, false)?;
    let (aimed, missing) = aimed_functions(&analysis, &request.focus)?;
    let tests = select::discover_tests(&mut analysis.project, &analysis.graph, &analysis.api);
    let (selected, without_tests) =
        select::select_tests(&analysis.graph, &tests, &aimed, request.max_tests);

    let mut planned: Vec<PlannedRun> = selected.iter().map(|s| planned_test(root, s)).collect();
    let mut harnesses: Vec<HarnessPlan> = Vec::new();
    let mut uncovered: Vec<Uncovered> = missing
        .into_iter()
        .map(|(function, reason)| Uncovered {
            function,
            file: String::new(),
            line: 0,
            reason,
        })
        .collect();
    for &function in &without_tests {
        let span = &analysis.graph.functions[function];
        let reason = if request.harness {
            match plan_harness(&analysis, root, function) {
                Ok((harness, runs)) => {
                    if !harnesses.iter().any(|h| h.target == harness.target) {
                        let room = request.max_tests.saturating_sub(planned.len());
                        planned.extend(runs.into_iter().take(room));
                        harnesses.push(harness);
                    } else if let Some(existing) =
                        harnesses.iter().find(|h| h.target == harness.target)
                    {
                        // Another aimed function shares the harness.
                        let name = span.qualified_name.clone();
                        let target = existing.target.clone();
                        for run in planned.iter_mut() {
                            if run.kind == RunKind::Harness
                                && run.target == TestTarget::Integration(target.clone())
                                && !run.reaches.contains(&name)
                            {
                                run.reaches.push(name.clone());
                            }
                        }
                    }
                    None
                }
                Err(reason) => Some(format!("no test reaches it; {reason}")),
            }
        } else {
            Some("no test reaches it (harness generation off)".to_string())
        };
        if let Some(reason) = reason {
            uncovered.push(Uncovered {
                function: span.qualified_name.clone(),
                file: span.file.clone(),
                line: span.start_line,
                reason,
            });
        }
    }
    let aimed_report: Vec<Aimed> = aimed
        .iter()
        .map(|&i| {
            let span = &analysis.graph.functions[i];
            Aimed {
                function: span.qualified_name.clone(),
                file: span.file.clone(),
                line: span.start_line,
            }
        })
        .collect();
    let mut report = MiriReport {
        miriflags: request.options.miriflags(),
        dry_run: request.dry_run,
        aimed: aimed_report,
        tests_found: tests.len(),
        planned: planned.clone(),
        harnesses: harnesses.clone(),
        runs: Vec::new(),
        findings: Vec::new(),
        confirmed: Vec::new(),
        uncovered,
        summary: Summary::default(),
        note: NOTE.to_string(),
    };
    if request.dry_run || planned.is_empty() {
        return Ok(report);
    }
    if !run::miri_available() {
        return Err(run::INSTALL_HINT.to_string());
    }
    for harness in &harnesses {
        let Some(callable) = harness_callable(&analysis, &harness.calls) else {
            continue;
        };
        let text = harness::HarnessText {
            target: harness.target.clone(),
            source: harness.source.clone(),
            cases: harness.cases.clone(),
        };
        harness::write(root, &callable, &text, Path::new(&harness.dir))?;
    }

    let logs = root.join(".codegraph").join("miri").join("logs");
    std::fs::create_dir_all(&logs).map_err(|e| format!("cannot create {}: {e}", logs.display()))?;
    let mut built: HashMap<Binary, Invocation> = HashMap::new();
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    for run in planned {
        let binary = Binary {
            manifest: run.manifest.clone(),
            target: run.target.clone(),
        };
        if !built.contains_key(&binary) {
            let log = logs.join(format!(
                "{}-build.log",
                log_name(&run.package, &run.target, "")
            ));
            let invocation = run::build(&binary, &request.options, &log)?;
            built.insert(binary.clone(), invocation);
        }
        let build = &built[&binary];
        let invocation = if build.status == RunStatus::Clean {
            let log = logs.join(format!(
                "{}.log",
                log_name(&run.package, &run.target, &run.test)
            ));
            run::run_test(&binary, Some(&run.test), &request.options, &log)?
        } else {
            build.clone()
        };
        let base = run.manifest.parent().map(Path::to_path_buf);
        let mapped = map_run(
            &analysis.project,
            root,
            &canonical,
            base.as_deref(),
            run,
            invocation,
        );
        report.runs.push(mapped);
    }
    finish(cg, &mut analysis.project, root, &mut report);
    Ok(report)
}

/// Map a Miri log (a `cargo miri test`/`run` output you ran yourself)
/// onto the project indexed at `root`.
pub fn report_from_log(cg: &CodeGraph, root: &Path, output: &str) -> Result<MiriReport, String> {
    let mut project = Project::load(cg, root)?;
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let parsed = parse_output(output);
    // A log does not say how the process exited: a test result or a Miri
    // report decides, else a compile error or missing dependency is a
    // failed build, and anything else is unrecognized.
    let (status, reason) = if parsed.primary().is_some() || parsed.ran_tests {
        let passed = parsed.failed == 0 && parsed.panics.is_empty();
        let status = run::status_of(&parsed, passed, false, false);
        let reason = match status {
            RunStatus::TestFailed => parsed
                .panics
                .first()
                .map(|p| format!("panicked: {}", p.message)),
            RunStatus::Unsupported | RunStatus::Aborted | RunStatus::Deadlock => {
                parsed.primary().map(|d| d.message.clone())
            }
            _ => None,
        };
        (status, reason)
    } else {
        let reason = run::build_reason(output, RunStatus::BuildFailed);
        let offline = reason.starts_with("a dependency is not in the local cargo cache");
        if offline || run::is_compile_error(output) {
            (RunStatus::BuildFailed, Some(reason))
        } else {
            (
                RunStatus::Error,
                Some("the log holds no Miri report and no test result".to_string()),
            )
        }
    };
    let test = parsed
        .primary()
        .and_then(|d| d.thread.clone())
        .or_else(|| parsed.failed_tests.first().cloned())
        .unwrap_or_else(|| "log".to_string());
    let planned = PlannedRun {
        kind: RunKind::Log,
        package: String::new(),
        target: TestTarget::Lib,
        test,
        file: String::new(),
        line: 0,
        calls: None,
        reaches: Vec::new(),
        distance: 0,
        manifest: root.join("Cargo.toml"),
    };
    let invocation = Invocation {
        status,
        command: String::new(),
        elapsed_secs: 0.0,
        parsed,
        log: String::new(),
        reason,
    };
    let run = map_run(&project, root, &canonical, Some(root), planned, invocation);
    let mut report = MiriReport {
        miriflags: String::new(),
        dry_run: false,
        aimed: Vec::new(),
        tests_found: 0,
        planned: Vec::new(),
        harnesses: Vec::new(),
        runs: vec![run],
        findings: Vec::new(),
        confirmed: Vec::new(),
        uncovered: Vec::new(),
        summary: Summary::default(),
        note: NOTE.to_string(),
    };
    finish(cg, &mut project, root, &mut report);
    Ok(report)
}

/// Findings, confirmations, the summary, and the saved report.
fn finish(cg: &CodeGraph, project: &mut Project, root: &Path, report: &mut MiriReport) {
    let mut proven: Vec<(String, String, String, String)> = Vec::new();
    for run in &report.runs {
        let (Some(diagnostic), Some(site)) = (&run.diagnostic, &run.site) else {
            continue;
        };
        if !diagnostic.category.is_proof() {
            continue;
        }
        let finding = finding_of(run, diagnostic, site);
        if let Some(id) = &site.function_id {
            proven.push((
                id.clone(),
                finding.rule.to_string(),
                format!("{}:{}", site.file, site.line),
                run.planned.test.clone(),
            ));
        }
        let duplicate = report
            .findings
            .iter()
            .any(|f| f.rule == finding.rule && f.file == finding.file && f.line == finding.line);
        if !duplicate {
            report.findings.push(finding);
        }
    }
    if !proven.is_empty() {
        report.confirmed = confirm_static(cg, project, &proven);
    }
    let mut summary = Summary {
        runs: report.runs.len(),
        ..Summary::default()
    };
    for run in &report.runs {
        let status = run.invocation.status;
        match status {
            RunStatus::Ub => summary.ub += 1,
            RunStatus::Leak => summary.leaks += 1,
            RunStatus::Clean => summary.clean += 1,
            RunStatus::TestFailed => summary.test_failed += 1,
            _ => summary.inconclusive += 1,
        }
        let key = serde_json::to_value(status)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        *summary.by_status.entry(key).or_default() += 1;
    }
    report.summary = summary;
    save(root, report);
}

/// The dense indices `focus` aims at, and the parts of a
/// [`MiriFocus::Several`] that named nothing (with why).
fn aimed_functions(
    analysis: &Analysis,
    focus: &MiriFocus,
) -> Result<(Vec<usize>, Vec<(String, String)>), String> {
    let one = |focus: &MiriFocus| -> Result<Vec<usize>, String> {
        match focus {
            MiriFocus::Finding { file, line } => analysis.resolve_focus(&Focus::Finding {
                file: file.clone(),
                line: *line,
            }),
            MiriFocus::Function(name) => analysis.resolve_focus(&Focus::Function(name.clone())),
            MiriFocus::AllUnsafe => Ok((0..analysis.graph.functions.len())
                .filter(|&i| {
                    let span = &analysis.graph.functions[i];
                    if span.is_test || !span.file.ends_with(".rs") {
                        return false;
                    }
                    let unsafe_fn = analysis.syntax.get(&span.id).is_some_and(|syntax| {
                        syntax.modifiers.split_whitespace().any(|m| m == "unsafe")
                    });
                    unsafe_fn || analysis.sites(i).unsafe_blocks > 0
                })
                .collect()),
            MiriFocus::Several(_) => Err("nested focus lists are not supported".into()),
        }
    };
    let MiriFocus::Several(parts) = focus else {
        return one(focus).map(|aimed| (aimed, Vec::new()));
    };
    let mut aimed: Vec<usize> = Vec::new();
    let mut missing: Vec<(String, String)> = Vec::new();
    for part in parts {
        match one(part) {
            Ok(found) => {
                for index in found {
                    if !aimed.contains(&index) {
                        aimed.push(index);
                    }
                }
            }
            Err(reason) => {
                let label = match part {
                    MiriFocus::Finding { file, line } => format!("{file}:{line}"),
                    MiriFocus::Function(name) => name.clone(),
                    _ => String::new(),
                };
                missing.push((label, reason));
            }
        }
    }
    if aimed.is_empty() {
        if let Some((_, reason)) = missing.first() {
            return Err(reason.clone());
        }
    }
    Ok((aimed, missing))
}

fn planned_test(root: &Path, selected: &Selected) -> PlannedRun {
    let test = &selected.test;
    PlannedRun {
        kind: RunKind::Test,
        package: test.package.clone(),
        target: test.target.clone(),
        test: test.path.clone(),
        file: test.file.clone(),
        line: test.line,
        calls: None,
        reaches: selected.reaches.clone(),
        distance: selected.distance,
        manifest: select::manifest_dir(root, &test.crate_dir).join("Cargo.toml"),
    }
}

/// How far up the callers a harness may start from an aimed function.
const HARNESS_DEPTH: u32 = 6;

/// A harness for aimed `function`: the function itself when an outside
/// crate can call it, else the nearest public function reaching it whose
/// inputs a harness can build.
fn plan_harness(
    analysis: &Analysis,
    root: &Path,
    function: usize,
) -> Result<(HarnessPlan, Vec<PlannedRun>), String> {
    let context = analysis.context();
    let mut first_error: Option<String> = None;
    for (candidate, distance) in analysis.graph.reverse_reach(function, HARNESS_DEPTH, 5000) {
        let span = &analysis.graph.functions[candidate];
        let Some(callable) = context.callable(span) else {
            continue;
        };
        match harness::render(&callable) {
            Ok(text) => {
                let dir = harness::harness_dir(root, &callable.krate.package);
                let aimed = analysis.graph.functions[function].qualified_name.clone();
                let runs = text
                    .cases
                    .iter()
                    .map(|case| PlannedRun {
                        kind: RunKind::Harness,
                        package: format!("{}-codegraph-miri", callable.krate.package),
                        target: TestTarget::Integration(text.target.clone()),
                        test: case.clone(),
                        file: callable.span.file.clone(),
                        line: callable.span.start_line,
                        calls: Some(callable.display_path()),
                        reaches: vec![aimed.clone()],
                        distance,
                        manifest: dir.join("Cargo.toml"),
                    })
                    .collect();
                return Ok((
                    HarnessPlan {
                        function: aimed,
                        calls: callable.display_path(),
                        target: text.target,
                        dir: dir.to_string_lossy().to_string(),
                        cases: text.cases,
                        source: text.source,
                    },
                    runs,
                ));
            }
            Err(reason) => {
                first_error.get_or_insert_with(|| format!("{}: {reason}", callable.display_path()));
            }
        }
    }
    Err(first_error.unwrap_or_else(|| {
        "no public function reaches it, so a harness in another crate cannot call it".to_string()
    }))
}

fn harness_callable(
    analysis: &Analysis,
    path: &str,
) -> Option<crate::analyze::fuzz::rust::RustCallable> {
    let context = analysis.context();
    analysis
        .graph
        .functions
        .iter()
        .filter_map(|span| context.callable(span))
        .find(|callable| callable.display_path() == path)
}

fn log_name(package: &str, target: &TestTarget, test: &str) -> String {
    let target = match target {
        TestTarget::Lib => "lib".to_string(),
        TestTarget::Integration(name) => format!("test-{name}"),
    };
    let raw = if test.is_empty() {
        format!("{package}-{target}")
    } else {
        format!("{package}-{target}-{test}")
    };
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// `file` (as Miri printed it: absolute, relative to the workspace cargo
/// ran in, or normalized) as an indexed project file.
fn map_file(
    project: &Project,
    root: &Path,
    canonical: &Path,
    base: Option<&Path>,
    file: &str,
) -> Option<String> {
    if let Some(base) = base {
        // Relative to the package or any workspace above it.
        let mut at = Some(base);
        while let Some(dir) = at {
            let candidate = dir.join(file);
            if candidate.is_file() {
                let candidate = candidate.canonicalize().unwrap_or(candidate);
                if let Some(found) =
                    project_file(project, root, canonical, &candidate.to_string_lossy())
                {
                    return Some(found);
                }
            }
            if dir == root || dir == canonical {
                break;
            }
            at = dir.parent();
        }
    }
    project_file(project, root, canonical, file)
}

fn site_at(
    project: &Project,
    root: &Path,
    canonical: &Path,
    base: Option<&Path>,
    place: &Place,
    frame: Option<&str>,
) -> Option<Site> {
    let file = map_file(project, root, canonical, base, &place.file)?;
    let span = project.enclosing_function(&file, place.line);
    Some(Site {
        line: place.line,
        col: place.col,
        function: span.map(|s| s.qualified_name.clone()),
        function_id: span.map(|s| s.id.clone()),
        frame: frame.map(str::to_string),
        file,
    })
}

fn map_run(
    project: &Project,
    root: &Path,
    canonical: &Path,
    base: Option<&Path>,
    planned: PlannedRun,
    invocation: Invocation,
) -> RunReport {
    let diagnostic = invocation.parsed.primary().cloned();
    let mut frames = Vec::new();
    let mut site = None;
    if let Some(diagnostic) = &diagnostic {
        for frame in &diagnostic.frames {
            let Some(mapped) = site_at(
                project,
                root,
                canonical,
                base,
                &frame.place,
                Some(&frame.function),
            ) else {
                continue;
            };
            // A test's `{closure#0}` frame is the test again.
            let repeat = frames.last().is_some_and(|last: &Site| {
                last.function.is_some() && last.function == mapped.function
            });
            if !repeat {
                frames.push(mapped);
            }
        }
        site = diagnostic
            .primary
            .as_ref()
            .and_then(|place| site_at(project, root, canonical, base, place, None))
            .or_else(|| frames.first().cloned());
    } else if let Some(panic) = invocation.parsed.panics.first() {
        site = panic
            .place
            .as_ref()
            .and_then(|place| site_at(project, root, canonical, base, place, None));
    }
    RunReport {
        planned,
        invocation,
        diagnostic,
        site,
        frames,
    }
}

fn finding_of(run: &RunReport, diagnostic: &MiriDiagnostic, site: &Site) -> Finding {
    let mut evidence: Vec<Evidence> = run
        .frames
        .iter()
        .enumerate()
        .filter(|(_, frame)| !(frame.file == site.file && frame.line == site.line))
        .map(|(depth, frame)| Evidence {
            file: frame.file.clone(),
            line: frame.line,
            note: format!(
                "Miri frame #{depth}: {}",
                frame.frame.as_deref().unwrap_or("?")
            ),
        })
        .collect();
    for related in &diagnostic.related {
        let note = related.note.clone();
        if let Some(frame) = run
            .frames
            .iter()
            .find(|f| related.place.file.ends_with(&f.file) && f.line == related.place.line)
        {
            evidence.push(Evidence {
                file: frame.file.clone(),
                line: frame.line,
                note,
            });
        } else if !related.place.file.starts_with('/') && related.place.line > 0 {
            evidence.push(Evidence {
                file: related.place.file.clone(),
                line: related.place.line,
                note,
            });
        }
    }
    let source = match run.planned.kind {
        RunKind::Test => format!("test `{}`", run.planned.test),
        RunKind::Harness => format!(
            "harness input `{}` calling `{}`",
            run.planned.test,
            run.planned.calls.as_deref().unwrap_or("?")
        ),
        RunKind::Log => "a Miri log".to_string(),
    };
    Finding {
        detector: Detector::Miri,
        rule: Cow::Owned(diagnostic.kind.rule()),
        file: site.file.clone(),
        line: site.line,
        col: site.col,
        function: site.function.clone(),
        message: format!("Miri: {} (in {source})", diagnostic.message),
        confidence: 1.0,
        evidence,
    }
}

/// Static findings (built-in detectors and rules) inside the functions
/// Miri proved UB in: `(function id, miri rule, place, test)`.
fn confirm_static(
    cg: &CodeGraph,
    project: &mut Project,
    proven: &[(String, String, String, String)],
) -> Vec<Confirmed> {
    let options = BugsOptions {
        include_tests: true,
        ..BugsOptions::default()
    };
    let mut findings = bugs::detect(project, &options);
    let rules = crate::analyze::rules::RuleSet::load(&[], &[], true);
    if let Ok(found) = crate::analyze::rules::detect(cg, project, &rules, &options) {
        findings.extend(found);
    }
    let mut seen: HashSet<(String, String, u32)> = HashSet::new();
    let mut confirmed = Vec::new();
    for finding in findings {
        let Some(id) = project
            .enclosing_function(&finding.file, finding.line)
            .map(|s| s.id.clone())
        else {
            continue;
        };
        for (function, rule, at, test) in proven {
            if *function == id
                && seen.insert((finding.rule.to_string(), finding.file.clone(), finding.line))
            {
                confirmed.push(Confirmed {
                    finding: finding.clone(),
                    by: rule.clone(),
                    at: at.clone(),
                    test: test.clone(),
                });
            }
        }
    }
    confirmed
}

/// The saved Miri findings `analyze review` reads.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedMiri {
    /// Seconds since the Unix epoch.
    pub ran_at: u64,
    pub miriflags: String,
    pub findings: Vec<Finding>,
}

fn saved_path(root: &Path) -> PathBuf {
    root.join(".codegraph").join("miri").join("report.json")
}

/// Save `report`'s findings (replacing the last run's) when it ran
/// anything.
fn save(root: &Path, report: &MiriReport) {
    if report.runs.is_empty() || !root.join(".codegraph").is_dir() {
        return;
    }
    let saved = SavedMiri {
        ran_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        miriflags: report.miriflags.clone(),
        findings: report.findings.clone(),
    };
    let path = saved_path(root);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(&saved) {
        let temp = path.with_extension("json.tmp");
        if std::fs::write(&temp, text).is_ok() {
            let _ = std::fs::rename(&temp, &path);
        }
    }
}

/// The findings of the last `analyze miri` run on the project at `root`
/// (none when it never ran).
pub fn saved_findings(root: &Path) -> Vec<Finding> {
    std::fs::read_to_string(saved_path(root))
        .ok()
        .and_then(|text| serde_json::from_str::<SavedMiri>(&text).ok())
        .map(|saved| saved.findings)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;

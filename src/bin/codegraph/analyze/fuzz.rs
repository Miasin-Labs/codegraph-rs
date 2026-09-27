use std::path::PathBuf;
use std::time::Duration;

use codegraph::analyze::fuzz::model::{Harnessable, InputShape};
use codegraph::analyze::fuzz::run::{self, RunOptions, RunStatus};
use codegraph::analyze::fuzz::{
    Focus,
    HarnessOptions,
    TargetsOptions,
    TargetsReport,
    fuzz_harness,
    fuzz_targets,
    locate_crash,
};

use super::{
    CodeGraph,
    OpenOptions,
    bold,
    cyan,
    dim,
    error_msg,
    green,
    info,
    is_initialized,
    parse_int_js,
    print_report_json,
    process,
    red,
    resolve_project_path,
    white,
    yellow,
};

/// `FILE:LINE`.
fn parse_finding(at: &str) -> Result<(String, u32), String> {
    at.rsplit_once(':')
        .and_then(|(file, line)| Some((file.to_string(), line.parse().ok()?)))
        .ok_or_else(|| format!("expected FILE:LINE, got \"{at}\""))
}

fn open_graph(project_path: &std::path::Path) -> Result<CodeGraph, String> {
    if !is_initialized(project_path) {
        return Err(format!(
            "CodeGraph not initialized in {}",
            project_path.display()
        ));
    }
    CodeGraph::open(project_path, &OpenOptions::default()).map_err(|e| e.to_string())
}

/// codegraph analyze fuzz-targets [--finding F:L | --function Q] [--top N]
pub(crate) fn cmd_analyze_fuzz_targets(
    finding: Option<&str>,
    function: Option<&str>,
    no_findings: bool,
    top_arg: &str,
    path_arg: Option<&str>,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);
    let body = || -> Result<(), String> {
        let focus = match (finding, function) {
            (Some(at), _) => {
                let (file, line) = parse_finding(at)?;
                Some(Focus::Finding { file, line })
            }
            (None, Some(function)) => Some(Focus::Function(function.to_string())),
            (None, None) => None,
        };
        let options = TargetsOptions {
            focus,
            top: parse_int_js(top_arg).unwrap_or(20).max(1) as usize,
            findings: !no_findings,
        };
        let cg = open_graph(&project_path)?;
        let report = fuzz_targets(&cg, &project_path, &options);
        cg.close();
        let report = report?;
        if json {
            return print_report_json("fuzz-targets", &report);
        }
        print_targets(&report);
        Ok(())
    };
    if let Err(msg) = body() {
        error_msg(&format!("analyze fuzz-targets failed: {msg}"));
        process::exit(1);
    }
}

fn shape_label(shape: InputShape) -> &'static str {
    match shape {
        InputShape::Bytes => "bytes",
        InputShape::Reader => "reader",
        InputShape::Text => "text",
        InputShape::Structured => "arbitrary",
        InputShape::Fixed => "no fuzz input",
        InputShape::Opaque => "needs input",
    }
}

fn print_targets(report: &TargetsReport) {
    if report.targets.is_empty() {
        let why = if report.crates.is_empty() {
            "no Rust library crate found (fuzz targets need a lib crate)".to_string()
        } else if report.focus.is_some() {
            "no public function reaches the focus".to_string()
        } else {
            format!("no callable targets among {} functions", report.functions)
        };
        info(&why);
        return;
    }
    let shown = report.targets.len();
    println!(
        "{}",
        bold(&format!(
            "\nFuzz targets: {shown} of {} ({} callable of {} functions; {})\n",
            shown + report.omitted,
            report.callable,
            report.functions,
            report.crates.join(", ")
        ))
    );
    if let Some(focus) = &report.focus {
        println!(
            "{}",
            dim(&format!(
                "Reaching {} ({} public callers)\n",
                focus.functions.join(", "),
                focus.reached_by
            ))
        );
    }
    for target in &report.targets {
        let features = &target.features;
        let mut why: Vec<String> = Vec::new();
        if features.parser_name {
            why.push("parser name".into());
        }
        if features.own.unsafe_blocks > 0 {
            why.push(format!("{} unsafe", features.own.unsafe_blocks));
        }
        if features.reach_risk > 0.0 {
            why.push(format!("reach risk {:.1}", features.reach_risk));
        }
        if features.recursive {
            why.push("recursive".into());
        }
        if features.findings > 0 {
            why.push(format!("{} finding(s)", features.findings));
        }
        if features.fan_in > 0 {
            why.push(format!("fan-in {}", features.fan_in));
        }
        if let Some(d) = target.distance {
            why.push(format!("{d} call(s) from focus"));
        }
        if let Some(by) = &features.covered_by {
            why.push(format!("reached by fuzz target {by}"));
        }
        let harness = match target.harness {
            Harnessable::Complete => green("harness ready"),
            Harnessable::NeedsInput => yellow("harness has TODOs"),
            Harnessable::No => red("not harnessable"),
        };
        println!(
            "{:>3}. {} {} {}",
            target.rank,
            white(&target.path),
            dim(&format!("{}:{}", target.file, target.line)),
            dim(&format!("score {:.2}", target.score))
        );
        println!(
            "     {} · {} · {}",
            cyan(shape_label(target.input)),
            harness,
            why.join(", ")
        );
        if !target.reaches.is_empty() {
            println!(
                "     {}",
                dim(&format!("reaches {}", target.reaches.join(", ")))
            );
        }
    }
    if report.already_fuzzed > 0 {
        println!(
            "{}",
            dim(&format!(
                "\n  {} function(s) already called by fuzz targets ({}) left out",
                report.already_fuzzed,
                report.existing_targets.join(", ")
            ))
        );
    }
    println!();
    info(&report.note);
}

/// codegraph analyze fuzz-harness (--function Q | --finding F:L) [--out DIR]
pub(crate) fn cmd_analyze_fuzz_harness(
    function: Option<&str>,
    finding: Option<&str>,
    out: Option<&str>,
    force: bool,
    dry_run: bool,
    path_arg: Option<&str>,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);
    let body = || -> Result<(), String> {
        let options = HarnessOptions {
            function: function.map(str::to_string),
            finding: finding.map(parse_finding).transpose()?,
            out: out.map(PathBuf::from),
            force,
            dry_run,
        };
        let cg = open_graph(&project_path)?;
        let report = fuzz_harness(&cg, &project_path, &options);
        cg.close();
        let report = report?;
        if json {
            return print_report_json("fuzz-harness", &report);
        }
        if let Some(reaches) = &report.reaches {
            info(&format!(
                "{reaches} is not callable from outside the crate; fuzzing {} which reaches it",
                report.path
            ));
        }
        for file in &report.files {
            println!("{} {}", dim(file.status), file.path);
        }
        if dry_run || report.files.is_empty() {
            println!("{}", report.source);
        }
        for todo in &report.todo {
            println!("{} {todo}", yellow("TODO"));
        }
        println!("\n{} {}", green("Run:"), report.command);
        Ok(())
    };
    if let Err(msg) = body() {
        error_msg(&format!("analyze fuzz-harness failed: {msg}"));
        process::exit(1);
    }
}

/// codegraph analyze fuzz-run --target NAME [--seconds N] [--input FILE]
#[allow(clippy::too_many_arguments)]
pub(crate) fn cmd_analyze_fuzz_run(
    target: &str,
    seconds_arg: &str,
    input: Option<&str>,
    fuzz_dir: Option<&str>,
    sanitizer: Option<String>,
    build_timeout_arg: &str,
    path_arg: Option<&str>,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);
    let body = || -> Result<RunStatus, String> {
        let seconds = parse_int_js(seconds_arg).unwrap_or(60).clamp(1, 3600) as u64;
        let build_timeout = parse_int_js(build_timeout_arg)
            .unwrap_or(900)
            .clamp(10, 7200) as u64;
        let fuzz_dir = match fuzz_dir {
            Some(dir) => {
                let dir = PathBuf::from(dir);
                if dir.is_absolute() {
                    dir
                } else {
                    std::env::current_dir()
                        .map_err(|e| e.to_string())?
                        .join(dir)
                }
            }
            None => project_path.join("fuzz"),
        };
        let input = input
            .map(|p| std::fs::canonicalize(p).map_err(|e| format!("cannot read input {p}: {e}")))
            .transpose()?;
        let options = RunOptions {
            target: target.to_string(),
            seconds,
            fuzz_dir,
            input,
            build_timeout: Duration::from_secs(build_timeout),
            sanitizer,
        };
        if !json {
            info(&format!(
                "Fuzzing {target} for up to {seconds}s (build limit {build_timeout}s)…"
            ));
        }
        let outcome = run::run(&options)?;
        let site = if is_initialized(&project_path) && !outcome.parsed.frames.is_empty() {
            let cg = open_graph(&project_path)?;
            let site = locate_crash(&cg, &project_path, &outcome.parsed.frames);
            cg.close();
            site?
        } else {
            None
        };
        if json {
            print_report_json(
                "fuzz-run",
                &serde_json::json!({ "outcome": outcome, "site": site }),
            )?;
            return Ok(outcome.status);
        }
        let status = match outcome.status {
            RunStatus::NoCrash => green("no crash"),
            RunStatus::Crash => red("CRASH"),
            RunStatus::Timeout => red("TIMEOUT"),
            RunStatus::OutOfMemory => red("OUT OF MEMORY"),
            RunStatus::Leak => red("LEAK"),
            RunStatus::BuildFailed => yellow("build failed"),
            RunStatus::Killed => yellow("killed at the deadline"),
            RunStatus::Error => yellow("error"),
        };
        println!(
            "{} {} {}",
            bold(&status),
            white(&outcome.target),
            dim(&format!(
                "({:.0}s{})",
                outcome.elapsed_secs,
                outcome
                    .parsed
                    .runs
                    .map(|r| format!(", {r} runs"))
                    .unwrap_or_default()
            ))
        );
        if let Some(kind) = &outcome.parsed.kind {
            println!("  {} {kind}", dim("kind:"));
        }
        if let Some(message) = &outcome.parsed.message {
            println!("  {} {message}", dim("message:"));
        }
        if let Some(site) = &site {
            println!(
                "  {} {}:{}{}",
                dim("at:"),
                site.file,
                site.line,
                site.function
                    .as_deref()
                    .map(|f| format!(" in {f}"))
                    .unwrap_or_default()
            );
        }
        if let Some(reproducer) = &outcome.parsed.reproducer {
            println!("  {} {reproducer}", dim("reproducer:"));
            println!(
                "  {} codegraph analyze fuzz-run --target {} --input {reproducer}",
                dim("replay:"),
                outcome.target
            );
        }
        if let Some(error) = &outcome.build_error {
            println!("{error}");
        }
        println!("  {} {}", dim("log:"), outcome.log);
        Ok(outcome.status)
    };
    match body() {
        Ok(RunStatus::BuildFailed | RunStatus::Error) => process::exit(2),
        Ok(_) => {}
        Err(msg) => {
            error_msg(&format!("analyze fuzz-run failed: {msg}"));
            process::exit(if msg == run::INSTALL_HINT { 3 } else { 1 });
        }
    }
}

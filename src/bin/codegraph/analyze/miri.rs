use std::io::Read as _;
use std::path::PathBuf;
use std::time::Duration;

use codegraph::analyze::miri::run::{Borrows, INSTALL_HINT, MiriOptions, RunStatus};
use codegraph::analyze::miri::{
    MiriFocus,
    MiriReport,
    MiriRequest,
    RunKind,
    miri_report,
    report_from_log,
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

/// `codegraph analyze miri` arguments, as clap parsed them.
pub(crate) struct MiriArgs {
    pub finding: Option<String>,
    pub function: Option<String>,
    pub seconds: String,
    pub build_timeout: String,
    pub max_tests: String,
    pub borrows: String,
    pub permissive_provenance: bool,
    pub disable_isolation: bool,
    pub ignore_leaks: bool,
    pub miri_flag: Vec<String>,
    pub online: bool,
    pub target_dir: Option<String>,
    pub no_harness: bool,
    pub dry_run: bool,
    pub log: Option<String>,
    pub path: Option<String>,
    pub json: bool,
}

/// `FILE:LINE`.
fn parse_finding(at: &str) -> Result<(String, u32), String> {
    at.rsplit_once(':')
        .and_then(|(file, line)| Some((file.to_string(), line.parse().ok()?)))
        .ok_or_else(|| format!("expected FILE:LINE, got \"{at}\""))
}

fn request(args: &MiriArgs) -> Result<MiriRequest, String> {
    let focus = match (&args.finding, &args.function) {
        (Some(at), _) => {
            let (file, line) = parse_finding(at)?;
            MiriFocus::Finding { file, line }
        }
        (None, Some(function)) => MiriFocus::Function(function.clone()),
        (None, None) => MiriFocus::AllUnsafe,
    };
    let borrows = match args.borrows.as_str() {
        "stacked" => Borrows::Stacked,
        "tree" => Borrows::Tree,
        "none" => Borrows::None,
        other => {
            return Err(format!(
                "unknown --borrows \"{other}\" — stacked, tree or none"
            ));
        }
    };
    let target_dir = args
        .target_dir
        .as_ref()
        .map(|dir| {
            let dir = PathBuf::from(dir);
            if dir.is_absolute() {
                Ok(dir)
            } else {
                std::env::current_dir()
                    .map(|cwd| cwd.join(dir))
                    .map_err(|e| e.to_string())
            }
        })
        .transpose()?;
    Ok(MiriRequest {
        focus,
        max_tests: parse_int_js(&args.max_tests).unwrap_or(16).clamp(1, 1000) as usize,
        harness: !args.no_harness,
        dry_run: args.dry_run,
        options: MiriOptions {
            borrows,
            strict_provenance: !args.permissive_provenance,
            disable_isolation: args.disable_isolation,
            ignore_leaks: args.ignore_leaks,
            extra_flags: args.miri_flag.clone(),
            online: args.online,
            target_dir,
            build_timeout: Duration::from_secs(
                parse_int_js(&args.build_timeout)
                    .unwrap_or(900)
                    .clamp(10, 7200) as u64,
            ),
            run_timeout: Duration::from_secs(
                parse_int_js(&args.seconds).unwrap_or(120).clamp(1, 7200) as u64,
            ),
            ..MiriOptions::default()
        },
    })
}

/// codegraph analyze miri [--finding F:L | --function Q | --all-unsafe]
pub(crate) fn cmd_analyze_miri(args: MiriArgs) {
    let project_path = resolve_project_path(args.path.as_deref());
    let body = || -> Result<MiriReport, String> {
        if !is_initialized(&project_path) {
            return Err(format!(
                "CodeGraph not initialized in {}",
                project_path.display()
            ));
        }
        let cg =
            CodeGraph::open(&project_path, &OpenOptions::default()).map_err(|e| e.to_string())?;
        let report = match &args.log {
            Some(log) => {
                let mut text = String::new();
                if log == "-" {
                    std::io::stdin()
                        .read_to_string(&mut text)
                        .map_err(|e| format!("cannot read stdin: {e}"))?;
                } else {
                    text = std::fs::read_to_string(log)
                        .map_err(|e| format!("cannot read {log}: {e}"))?;
                }
                report_from_log(&cg, &project_path, &text)
            }
            None => {
                let request = request(&args)?;
                if !args.json && !request.dry_run {
                    info(&format!(
                        "Running Miri (MIRIFLAGS=\"{}\", up to {} test(s), {}s each)…",
                        request.options.miriflags(),
                        request.max_tests,
                        request.options.run_timeout.as_secs()
                    ));
                }
                miri_report(&cg, &project_path, &request)
            }
        };
        cg.close();
        report
    };
    match body() {
        Ok(report) => {
            if args.json {
                if let Err(msg) = print_report_json("miri", &report) {
                    error_msg(&msg);
                    process::exit(1);
                }
            } else {
                print_report(&report);
            }
            if report.summary.ub + report.summary.leaks > 0 {
                process::exit(4);
            }
        }
        Err(msg) => {
            error_msg(&format!("analyze miri failed: {msg}"));
            process::exit(if msg == INSTALL_HINT { 3 } else { 1 });
        }
    }
}

fn status_label(status: RunStatus) -> String {
    match status {
        RunStatus::Ub => red("UB"),
        RunStatus::Leak => red("LEAK"),
        RunStatus::Clean => green("clean"),
        RunStatus::TestFailed => yellow("test failed"),
        RunStatus::Unsupported => yellow("unsupported"),
        RunStatus::Aborted => yellow("aborted"),
        RunStatus::Deadlock => yellow("deadlock"),
        RunStatus::BuildFailed => yellow("build failed"),
        RunStatus::Timeout => yellow("timeout"),
        RunStatus::OutputCapped => yellow("output capped"),
        RunStatus::Ignored => dim("ignored"),
        RunStatus::NoTest => dim("no such test"),
        RunStatus::Error => yellow("error"),
    }
}

fn print_report(report: &MiriReport) {
    if !report.aimed.is_empty() {
        let names: Vec<&str> = report
            .aimed
            .iter()
            .take(8)
            .map(|a| a.function.as_str())
            .collect();
        println!(
            "{}",
            bold(&format!(
                "\nMiri: {} aimed function(s){} — {} test(s) in the project",
                report.aimed.len(),
                if names.is_empty() {
                    String::new()
                } else {
                    format!(
                        " ({}{})",
                        names.join(", "),
                        if report.aimed.len() > names.len() {
                            ", …"
                        } else {
                            ""
                        }
                    )
                },
                report.tests_found
            ))
        );
    }
    if report.dry_run {
        println!("{}", white("Plan (dry run, nothing run or written):"));
        for run in &report.planned {
            let what = match run.kind {
                RunKind::Harness => format!(
                    "harness {} input {} → {}",
                    target_name(&run.target),
                    run.test,
                    run.calls.as_deref().unwrap_or("?")
                ),
                _ => format!("{} {}", run.package, run.test),
            };
            println!(
                "  {} {}",
                cyan(&what),
                dim(&format!(
                    "reaches {} ({} call(s))",
                    run.reaches.join(", "),
                    run.distance
                ))
            );
        }
        for harness in &report.harnesses {
            println!(
                "  {} {} for {} ({} inputs) in {}",
                dim("harness"),
                harness.target,
                harness.function,
                harness.cases.len(),
                harness.dir
            );
        }
    }
    for run in &report.runs {
        let name = match run.planned.kind {
            RunKind::Harness => format!(
                "{}::{} ({})",
                target_name(&run.planned.target),
                run.planned.test,
                run.planned.calls.as_deref().unwrap_or("?")
            ),
            _ => run.planned.test.clone(),
        };
        println!(
            "{} {} {}",
            bold(&status_label(run.invocation.status)),
            white(&name),
            dim(&format!("({:.0}s)", run.invocation.elapsed_secs))
        );
        if let Some(diagnostic) = &run.diagnostic {
            println!("  {} {}", dim(diagnostic.kind.id()), diagnostic.message);
        }
        if let Some(site) = &run.site {
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
        for frame in run.frames.iter().skip(1).take(6) {
            println!(
                "  {} {}:{} {}",
                dim("from"),
                frame.file,
                frame.line,
                dim(frame.frame.as_deref().unwrap_or(""))
            );
        }
        if let Some(reason) = &run.invocation.reason {
            for line in reason.lines().take(12) {
                println!("  {}", dim(line));
            }
        }
        if !run.invocation.log.is_empty() && run.invocation.status != RunStatus::Clean {
            println!("  {} {}", dim("log:"), run.invocation.log);
        }
    }
    if !report.confirmed.is_empty() {
        println!("\n{}", white("Static findings Miri confirms:"));
        for confirmed in &report.confirmed {
            println!(
                "  {} {}:{} {}",
                red(&confirmed.finding.rule),
                confirmed.finding.file,
                confirmed.finding.line,
                dim(&format!(
                    "({} at {}, {})",
                    confirmed.by, confirmed.at, confirmed.test
                ))
            );
        }
    }
    if !report.uncovered.is_empty() {
        println!("\n{}", white("Not run (nothing proven):"));
        for uncovered in report.uncovered.iter().take(20) {
            println!(
                "  {} {}",
                uncovered.function,
                dim(&format!(
                    "{}:{} — {}",
                    uncovered.file, uncovered.line, uncovered.reason
                ))
            );
        }
        if report.uncovered.len() > 20 {
            println!(
                "  {}",
                dim(&format!("… {} more", report.uncovered.len() - 20))
            );
        }
    }
    let summary = &report.summary;
    if summary.runs > 0 {
        println!(
            "\n{} run(s): {} UB, {} leak(s), {} clean, {} failed, {} inconclusive",
            summary.runs,
            summary.ub,
            summary.leaks,
            summary.clean,
            summary.test_failed,
            summary.inconclusive
        );
    } else if !report.dry_run {
        info("Nothing to run: no test or harness reaches the aimed functions");
    }
    println!();
    info(&report.note);
}

fn target_name(target: &codegraph::analyze::miri::select::TestTarget) -> String {
    match target {
        codegraph::analyze::miri::select::TestTarget::Lib => "lib".to_string(),
        codegraph::analyze::miri::select::TestTarget::Integration(name) => name.clone(),
    }
}

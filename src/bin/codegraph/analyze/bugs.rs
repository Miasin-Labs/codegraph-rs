use codegraph::analyze::bugs::{
    BugsOptions,
    Detector,
    ReviewPacket,
    ReviewSelection,
    bugs_report,
    bugs_review,
};

use super::{
    CodeGraph,
    OpenOptions,
    analysis_reports,
    bold,
    cyan,
    dim,
    error_msg,
    format_number,
    green,
    info,
    is_initialized,
    parse_int_js,
    print_report_json,
    process,
    resolve_project_path,
    white,
    yellow,
};

/// codegraph analyze bugs [--base REV] [--detector deviance|lint] [--tests]
pub(crate) fn cmd_analyze_bugs(
    base: Option<&str>,
    detectors: &[String],
    include_tests: bool,
    top_arg: &str,
    path_arg: Option<&str>,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        if !is_initialized(&project_path) {
            return Err(format!(
                "CodeGraph not initialized in {}",
                project_path.display()
            ));
        }
        let options = bugs_options(&project_path, base, detectors, include_tests)?;
        let top = parse_int_js(top_arg).unwrap_or(50).max(1) as usize;

        let cg =
            CodeGraph::open(&project_path, &OpenOptions::default()).map_err(|e| e.to_string())?;
        let report = bugs_report(&cg, &project_path, &options);
        cg.close();
        let mut report = report?;
        report.limit(top);

        if json {
            return print_report_json("bugs", &report);
        }
        if report.findings.is_empty() {
            info(&format!(
                "No findings across {} files and {} call sites{}",
                format_number(report.files_scanned as u64),
                format_number(report.call_sites as u64),
                base.map(|b| format!(" (changes since {b})"))
                    .unwrap_or_default()
            ));
            return Ok(());
        }
        let shown = report.findings.len();
        println!(
            "{}",
            bold(&format!(
                "\nSuspected bugs: {} of {} ({} files, {} call sites)\n",
                shown,
                shown + report.findings_omitted,
                format_number(report.files_scanned as u64),
                format_number(report.call_sites as u64)
            ))
        );
        for (index, finding) in report.findings.iter().enumerate() {
            println!(
                "{:>3}. {} {} {}",
                index + 1,
                yellow(finding.rule),
                white(&format!("{}:{}", finding.file, finding.line)),
                dim(&format!(
                    "({:.0}%{})",
                    finding.confidence * 100.0,
                    finding
                        .function
                        .as_deref()
                        .map(|f| format!(", in {f}"))
                        .unwrap_or_default()
                ))
            );
            println!("     {}", finding.message);
            for evidence in finding.evidence.iter().take(4) {
                println!(
                    "     {} {}",
                    cyan(&format!("{}:{}", evidence.file, evidence.line)),
                    dim(&evidence.note)
                );
            }
            if finding.evidence.len() > 4 {
                println!(
                    "     {}",
                    dim(&format!("… {} more", finding.evidence.len() - 4))
                );
            }
        }
        if report.findings_omitted > 0 {
            println!(
                "{}",
                dim(&format!(
                    "\n  … {} more (raise --top)",
                    report.findings_omitted
                ))
            );
        }
        println!();
        info(&report.note);
        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze bugs failed: {msg}"));
        process::exit(1);
    }
}

fn bugs_options(
    project_path: &std::path::Path,
    base: Option<&str>,
    detectors: &[String],
    include_tests: bool,
) -> Result<BugsOptions, String> {
    let detectors = detectors
        .iter()
        .map(|name| match name.as_str() {
            "deviance" => Ok(Detector::Deviance),
            "lint" => Ok(Detector::Lint),
            other => Err(format!(
                "unknown detector \"{other}\" — known: deviance, lint"
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let only_files = base
        .map(|base| analysis_reports::changed_files(project_path, base))
        .transpose()?;
    Ok(BugsOptions {
        detectors,
        only_files,
        include_tests,
    })
}

/// `FILE` or `FILE:LINE`.
fn parse_at(at: &str) -> (String, Option<u32>) {
    match at.rsplit_once(':') {
        Some((file, line)) if line.parse::<u32>().is_ok() => (file.to_string(), line.parse().ok()),
        _ => (at.to_string(), None),
    }
}

/// codegraph analyze review [--at FILE[:LINE]] [--rule R] [--base REV]
#[allow(clippy::too_many_arguments)]
pub(crate) fn cmd_analyze_review(
    at: Option<&str>,
    rule: Option<String>,
    base: Option<&str>,
    detectors: &[String],
    include_tests: bool,
    top_arg: &str,
    path_arg: Option<&str>,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        if !is_initialized(&project_path) {
            return Err(format!(
                "CodeGraph not initialized in {}",
                project_path.display()
            ));
        }
        let options = bugs_options(&project_path, base, detectors, include_tests)?;
        let selection = ReviewSelection {
            rule,
            at: at.map(parse_at),
            top: parse_int_js(top_arg).unwrap_or(10).max(1) as usize,
        };

        let cg =
            CodeGraph::open(&project_path, &OpenOptions::default()).map_err(|e| e.to_string())?;
        let packets = bugs_review(&cg, &project_path, &options, &selection);
        cg.close();
        let packets = packets?;

        if json {
            return print_report_json("review", &packets);
        }
        if packets.is_empty() {
            info("No findings to review");
            return Ok(());
        }
        for (index, packet) in packets.iter().enumerate() {
            print_packet(index + 1, packets.len(), packet);
        }
        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze review failed: {msg}"));
        process::exit(1);
    }
}

fn print_packet(number: usize, total: usize, packet: &ReviewPacket) {
    let finding = &packet.finding;
    println!(
        "{}",
        bold(&format!(
            "\n── Review {number}/{total}: {} at {}:{} ({:.0}%)",
            finding.rule,
            finding.file,
            finding.line,
            finding.confidence * 100.0
        ))
    );
    if let Some(function) = &finding.function {
        println!("{}", dim(&format!("in {function}")));
    }
    println!("{}\n", finding.message);
    if let Some(snippet) = &packet.function {
        println!(
            "{}",
            white(&format!(
                "{}:{}-{}",
                snippet.file, snippet.start_line, snippet.end_line
            ))
        );
        println!("{}\n", snippet.source);
    }
    for snippet in &packet.evidence {
        println!(
            "{} {}",
            cyan(&format!("{}:{}", snippet.file, snippet.start_line)),
            dim(snippet.note.as_deref().unwrap_or(""))
        );
        println!("{}\n", snippet.source);
    }
    if !packet.callers.is_empty() {
        println!("{}", white("Called from:"));
        for caller in &packet.callers {
            println!(
                "  {} {}",
                caller.name,
                dim(&format!("{}:{}", caller.file, caller.line))
            );
        }
        println!();
    }
    println!("{}", yellow("Decide:"));
    for question in &packet.checklist {
        println!("  {} {question}", green("□"));
    }
}

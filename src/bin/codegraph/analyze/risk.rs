use super::{
    CodeGraph,
    OpenOptions,
    analysis_reports,
    bold,
    dim,
    error_msg,
    format_number,
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

/// codegraph analyze risk [--base REV] [--depth N] [--test-depth N] [--untested]
#[allow(clippy::too_many_arguments)]
pub(crate) fn cmd_analyze_risk(
    base: &str,
    depth_arg: &str,
    test_depth_arg: &str,
    untested: bool,
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
        let depth = parse_int_js(depth_arg)
            .unwrap_or(i64::from(analysis_reports::RISK_IMPACT_DEPTH))
            .clamp(1, 10) as u32;
        let test_depth = parse_int_js(test_depth_arg)
            .unwrap_or(i64::from(analysis_reports::RISK_TEST_DEPTH))
            .clamp(1, 10) as u32;
        let top = parse_int_js(top_arg).unwrap_or(30).max(1) as usize;

        let cg =
            CodeGraph::open(&project_path, &OpenOptions::default()).map_err(|e| e.to_string())?;
        let report = analysis_reports::risk_report(&cg, &project_path, base, depth, test_depth);
        cg.close();
        let mut report = report?;
        report.limit(top, untested);

        if json {
            return print_report_json("risk", &report);
        }
        if report.functions == 0 {
            info(&format!(
                "No indexed function or method changed since {} (test files excluded)",
                report.base
            ));
            return Ok(());
        }

        let tested = report.functions - report.untested;
        println!(
            "{}",
            bold(&format!(
                "\nChange risk since {}: {} functions in {} files — {} reached by no test, {} tested\n",
                report.base,
                format_number(report.functions as u64),
                format_number(report.changed_files as u64),
                format_number(report.untested as u64),
                format_number(tested as u64),
            ))
        );
        println!(
            "{}",
            dim(&format!("  {:>10}  {:>6}  symbol", "dependents", "tests"))
        );
        for entry in &report.entries {
            let tests = if entry.tests == 0 {
                red("none")
            } else if entry.tests * 4 < entry.dependents {
                yellow(&entry.tests.to_string())
            } else {
                green(&entry.tests.to_string())
            };
            println!(
                "  {:>10}  {:>6}  {} {}",
                entry.dependents,
                tests,
                white(&entry.qualified_name),
                dim(&format!("{}:{}", entry.file, entry.line))
            );
        }
        if report.entries_omitted > 0 {
            println!(
                "{}",
                dim(&format!(
                    "  … {} more (raise --top{})",
                    report.entries_omitted,
                    if untested { "" } else { ", or --untested" }
                ))
            );
        }
        println!();
        info(&format!(
            "dependents: impact radius at depth {}; tests: test functions within {} hops. {}",
            report.impact_depth, report.test_depth, report.note
        ));
        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze risk failed: {msg}"));
        process::exit(1);
    }
}

use super::*;

/// codegraph analyze coverage --lcov <path> [--untested]
pub(crate) fn cmd_analyze_coverage(
    lcov: &str,
    untested_only: bool,
    top_arg: &str,
    path_arg: Option<&str>,
    no_cache: bool,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let mut bridged = bridge_project(&project_path, no_cache, json)?;
        let top = parse_int_js(top_arg).unwrap_or(50).max(1) as usize;
        let report = analysis_reports::coverage_report(
            &mut bridged.graph,
            Path::new(lcov),
            &project_path,
            untested_only,
            top,
        )?;

        if json {
            return print_report_json("coverage", &report);
        }

        println!(
            "{}",
            bold(&format!(
                "\nCoverage: {} tested / {} untested of {} functions ({} LCOV files):\n",
                format_number(report.functions_tested as u64),
                format_number(report.functions_untested as u64),
                format_number(report.functions_total as u64),
                report.lcov_files
            ))
        );
        for function in &report.functions {
            print_symbol_line(
                &function.symbol.kind,
                &function.symbol.name,
                &function.symbol.file,
                function.symbol.line,
            );
            println!(
                "{}",
                if function.tested {
                    dim(&format!("  tested ({} hits)", function.coverage_count))
                } else {
                    yellow("  untested")
                }
            );
            println!();
        }
        if report.truncated {
            info(&format!(
                "Listing capped at {} {} raise with --top for more",
                report.functions.len(),
                get_glyphs().dash
            ));
        }
        if report.parse_warnings > 0 {
            warn(&format!(
                "{} malformed LCOV lines skipped",
                report.parse_warnings
            ));
        }
        info(&report.note);

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze coverage failed: {msg}"));
        process::exit(1);
    }
}

use super::{
    analysis_reports,
    bold,
    bridge_project,
    dim,
    error_msg,
    format_number,
    get_glyphs,
    info,
    js_to_fixed,
    parse_int_js,
    print_report_json,
    print_symbol_line,
    process,
    resolve_project_path,
};

/// codegraph analyze complexity [--top N]
pub(crate) fn cmd_analyze_complexity(
    path_arg: Option<&str>,
    top_arg: &str,
    no_cache: bool,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let top = parse_int_js(top_arg).unwrap_or(20).max(1) as usize;
        let report = analysis_reports::complexity_report(&bridged.graph, &project_path, top);

        if json {
            return print_report_json("complexity", &report);
        }

        if report.functions.is_empty() {
            info("No functions with complexity metrics found (run with --json for skip reasons)");
            return Ok(());
        }

        println!(
            "{}",
            bold(&format!(
                "\nMost complex functions (top {} of {} analyzed):\n",
                report.functions.len(),
                format_number(report.functions_analyzed as u64)
            ))
        );
        for f in &report.functions {
            let mi = f
                .maintainability_index
                .map(|v| format!(", MI {}", js_to_fixed(v, 0)))
                .unwrap_or_default();
            print_symbol_line(
                &f.symbol.kind,
                &f.symbol.name,
                &f.symbol.file,
                f.symbol.line,
            );
            println!(
                "{}",
                dim(&format!(
                    "  cyclomatic {}, cognitive {}, nesting {}{mi}",
                    f.cyclomatic, f.cognitive, f.max_nesting
                ))
            );
            println!();
        }

        let skipped_total: usize = report.skipped.values().sum();
        if skipped_total > 0 {
            info(&format!(
                "{} of {} functions skipped (unsupported language or unreadable source) {} use --json for the breakdown",
                format_number(skipped_total as u64),
                format_number(
                    (report.functions_total
                        + report.skipped.get("placeholder").copied().unwrap_or(0))
                        as u64
                ),
                get_glyphs().dash
            ));
        }

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze complexity failed: {msg}"));
        process::exit(1);
    }
}

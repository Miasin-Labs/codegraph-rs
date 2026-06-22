use super::{
    analysis_reports,
    bold,
    bridge_project,
    cyan,
    dim,
    error_msg,
    info,
    parse_int_js,
    print_report_json,
    print_symbol_line,
    process,
    resolve_project_path,
    resolve_symbol_via_index,
    success,
    warn,
    white,
};

/// codegraph analyze validate <symbol> --params-before N --params-after M
pub(crate) fn cmd_analyze_validate(
    symbol: &str,
    params_before_arg: &str,
    params_after_arg: &str,
    path_arg: Option<&str>,
    no_cache: bool,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let parse_params = |label: &str, value: &str| -> usize {
        match parse_int_js(value) {
            Some(n) if n >= 0 => n as usize,
            _ => {
                error_msg(&format!(
                    "--{label} must be a non-negative integer (got \"{value}\")."
                ));
                process::exit(1);
            }
        }
    };
    let params_before = parse_params("params-before", params_before_arg);
    let params_after = parse_params("params-after", params_after_arg);

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let Some(target) = resolve_symbol_via_index(&project_path, &bridged.id_map, symbol)? else {
            info(&format!(
                "Symbol \"{symbol}\" not found in the analysis graph"
            ));
            return Ok(());
        };
        let Some(report) =
            analysis_reports::validate_report(&bridged.graph, &target, params_before, params_after)
        else {
            info(&format!(
                "Symbol \"{symbol}\" not found in the analysis graph"
            ));
            return Ok(());
        };

        if json {
            return print_report_json("validate", &report);
        }

        println!(
            "{}",
            bold(&format!(
                "\nSignature change for \"{symbol}\": {} -> {} parameter{}\n",
                report.params_before,
                report.params_after,
                if report.params_after == 1 { "" } else { "s" }
            ))
        );
        if report.is_safe {
            success(&format!(
                "Safe: no incompatible callers ({} compatible)",
                report.compatible.len()
            ));
        } else {
            warn(&format!(
                "Unsafe: {} caller{} updating",
                report.incompatible.len(),
                if report.incompatible.len() == 1 {
                    " needs"
                } else {
                    "s need"
                }
            ));
            println!();
            for caller in &report.incompatible {
                print_symbol_line(
                    &caller.symbol.kind,
                    &caller.symbol.name,
                    &caller.symbol.file,
                    caller.symbol.line,
                );
                println!("{}", dim(&format!("  {}", caller.reason)));
                println!();
            }
        }
        if !report.call_sites.is_empty() {
            println!("{}", cyan("Affected call sites:"));
            for site in &report.call_sites {
                let loc = if site.line != 0 {
                    format!(":{}", site.line)
                } else {
                    String::new()
                };
                println!(
                    "  {} {}",
                    white(&site.caller),
                    dim(&format!("{}{loc}", site.file))
                );
            }
            println!();
        }
        info(&report.note);

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze validate failed: {msg}"));
        process::exit(1);
    }
}

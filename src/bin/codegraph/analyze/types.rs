use super::*;

/// codegraph analyze types <symbol>
pub(crate) fn cmd_analyze_types(symbol: &str, path_arg: Option<&str>, no_cache: bool, json: bool) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let mut bridged = bridge_project(&project_path, no_cache, json)?;
        let Some(target) = resolve_symbol_via_index(&project_path, &bridged.id_map, symbol)? else {
            info(&format!(
                "Symbol \"{symbol}\" not found in the analysis graph"
            ));
            return Ok(());
        };
        let Some(report) = analysis_reports::types_report(&mut bridged.graph, &target)? else {
            info(&format!(
                "Symbol \"{symbol}\" not found in the analysis graph"
            ));
            return Ok(());
        };

        if json {
            return print_report_json("types", &report);
        }

        println!(
            "{}",
            bold(&format!("\nPossible concrete types for \"{symbol}\":\n"))
        );
        if report.input_types.is_empty() && report.return_types.is_empty() {
            info(&report.note);
            return Ok(());
        }
        if !report.input_types.is_empty() {
            println!(
                "{} {}",
                cyan("inputs: "),
                white(&report.input_types.join(", "))
            );
        }
        if !report.return_types.is_empty() {
            println!(
                "{} {}",
                cyan("returns:"),
                white(&report.return_types.join(", "))
            );
        }
        println!();
        info(&format!(
            "{} functions annotated by the propagation pass",
            format_number(report.functions_annotated as u64)
        ));
        info(&report.note);

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze types failed: {msg}"));
        process::exit(1);
    }
}

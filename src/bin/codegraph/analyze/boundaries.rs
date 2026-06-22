use super::{
    analysis_reports,
    bold,
    bridge_project,
    cyan,
    dim,
    error_msg,
    info,
    print_report_json,
    process,
    resolve_project_path,
    warn,
    white,
};

/// codegraph analyze boundaries
pub(crate) fn cmd_analyze_boundaries(path_arg: Option<&str>, no_cache: bool, json: bool) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let mut bridged = bridge_project(&project_path, no_cache, json)?;
        let report = analysis_reports::boundaries_report(&mut bridged.graph);

        if json {
            return print_report_json("boundaries", &report);
        }

        if report.boundary_count == 0 {
            warn(&report.note);
            return Ok(());
        }

        println!(
            "{}",
            bold(&format!(
                "\nCross-language boundaries ({}):\n",
                report.boundary_count
            ))
        );
        if !report.http_routes.is_empty() {
            println!("{}", cyan("HTTP routes:"));
            for route in &report.http_routes {
                println!(
                    "  {} {} {}",
                    white(&format!("{} {}", route.method, route.path)),
                    dim("->"),
                    white(&route.provider.name)
                );
            }
            println!();
        }
        if !report.ffi_exports.is_empty() {
            println!("{}", cyan("FFI exports (C ABI):"));
            for export in &report.ffi_exports {
                println!(
                    "  {} {}",
                    white(&export.symbol_name),
                    dim(&export.provider.file)
                );
            }
            println!();
        }
        if !report.wasm_boundaries.is_empty() {
            println!("{}", cyan("WASM boundaries:"));
            for boundary in &report.wasm_boundaries {
                let module = boundary
                    .module
                    .as_ref()
                    .map(|m| format!("{m}."))
                    .unwrap_or_default();
                println!(
                    "  {} {}{} {}",
                    dim(&boundary.direction),
                    dim(&module),
                    white(&boundary.name),
                    dim(&boundary.provider.file)
                );
            }
            println!();
        }
        info(&format!(
            "Cross-language stitching: {} clients seen, {} call edges emitted",
            report.cross_language_calls.clients_seen, report.cross_language_calls.edges_emitted
        ));
        info(&report.note);

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze boundaries failed: {msg}"));
        process::exit(1);
    }
}

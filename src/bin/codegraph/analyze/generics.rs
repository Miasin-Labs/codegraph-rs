use super::*;

/// codegraph analyze generics [symbol]
pub(crate) fn cmd_analyze_generics(
    symbol: Option<&str>,
    path_arg: Option<&str>,
    no_cache: bool,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let report = analysis_reports::generics_report(&bridged.graph, symbol);

        if json {
            return print_report_json("generics", &report);
        }

        if !report.instantiations.is_empty() {
            println!(
                "{}",
                bold(&format!(
                    "\nGeneric instantiations ({}):\n",
                    report.instantiation_count
                ))
            );
            for instantiation in &report.instantiations {
                println!(
                    "  {} {} {} {}",
                    white(&instantiation.generic.name),
                    dim("<-"),
                    white(&instantiation.callsite.name),
                    dim(&format!("[{}]", instantiation.type_args.join(", ")))
                );
            }
            println!();
        }
        if !report.likely_generic_definitions.is_empty() {
            println!(
                "{}",
                bold(&format!(
                    "\nLikely generic definitions ({}, signature heuristic):\n",
                    report.likely_generic_count
                ))
            );
            for definition in &report.likely_generic_definitions {
                print_symbol_line(
                    &definition.symbol.kind,
                    &definition.symbol.name,
                    &definition.symbol.file,
                    definition.symbol.line,
                );
                println!(
                    "{}",
                    dim(&format!(
                        "  type params: {}",
                        definition.type_params.join(", ")
                    ))
                );
                println!();
            }
        }
        if report.instantiations.is_empty() && report.likely_generic_definitions.is_empty() {
            match symbol {
                Some(filter) => info(&format!("No generic definition matches \"{filter}\"")),
                None => info("No generic definitions detected"),
            }
        }
        info(&report.note);

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze generics failed: {msg}"));
        process::exit(1);
    }
}

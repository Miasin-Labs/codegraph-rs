use super::*;

/// codegraph analyze dataflow <symbol>
///
/// Per-function dataflow facts (params, returns, assignments, argument
/// flows, mutations), same source re-parse anchoring as `analyze cfg`.
pub(crate) fn cmd_analyze_dataflow(
    symbol: &str,
    path_arg: Option<&str>,
    no_cache: bool,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let Some(target) = resolve_symbol_via_index(&project_path, &bridged.id_map, symbol)? else {
            info(&format!(
                "Symbol \"{symbol}\" not found in the analysis graph"
            ));
            return Ok(());
        };
        let Some(report) = analyze_ir::dataflow_report(&bridged.graph, &project_path, &target)
        else {
            info(&format!(
                "Symbol \"{symbol}\" not found in the analysis graph"
            ));
            return Ok(());
        };

        if json {
            return print_report_json("dataflow", &report);
        }

        if !report.analyzed {
            // Honest capability note — never empty fact sections.
            warn(&report.note);
            return Ok(());
        }

        println!(
            "{}",
            bold(&format!("\nDataflow of \"{}\":\n", report.symbol.name))
        );
        print_symbol_line(
            &report.symbol.kind,
            &report.symbol.name,
            &report.symbol.file,
            report.symbol.line,
        );
        println!();

        if !report.params.is_empty() {
            println!("{}", cyan("Params:"));
            for p in &report.params {
                let ty = p
                    .type_annotation
                    .as_deref()
                    .map(|t| format!(": {t}"))
                    .unwrap_or_default();
                let default = if p.has_default { " (has default)" } else { "" };
                println!(
                    "  {}{}",
                    white(&format!("{}{ty}", p.name)),
                    dim(&format!("  position {}{default}", p.position))
                );
            }
            println!();
        }
        if !report.assignments.is_empty() {
            println!("{}", cyan("Assignments:"));
            for a in &report.assignments {
                println!(
                    "  {} {}",
                    white(&a.target),
                    dim(&format!("<- {} (line {})", a.source_kind, a.line))
                );
            }
            println!();
        }
        if !report.returns.is_empty() {
            println!("{}", cyan("Returns:"));
            for r in &report.returns {
                println!(
                    "  {} {}",
                    white(&r.expression),
                    dim(&format!("(line {})", r.line))
                );
            }
            println!();
        }
        if !report.arg_flows.is_empty() {
            println!("{}", cyan("Argument flows:"));
            for f in &report.arg_flows {
                let from = f
                    .source_param
                    .as_deref()
                    .map(|p| format!("param {p} -> "))
                    .unwrap_or_default();
                println!(
                    "  {} {}",
                    white(&format!("{from}{} arg {}", f.callee, f.arg_position)),
                    dim(&format!("(line {})", f.line))
                );
            }
            println!();
        }
        if !report.mutations.is_empty() {
            println!("{}", cyan("Mutations:"));
            for m in &report.mutations {
                println!(
                    "  {} {}",
                    white(&format!("{}.{}", m.target, m.method)),
                    dim(&format!("(line {})", m.line))
                );
            }
            println!();
        }
        if report.params.is_empty()
            && report.assignments.is_empty()
            && report.returns.is_empty()
            && report.arg_flows.is_empty()
            && report.mutations.is_empty()
        {
            info(&format!(
                "No dataflow facts in \"{}\" {} the body has no params, assignments, returns, \
                 argument flows, or mutations",
                report.symbol.name,
                get_glyphs().dash
            ));
        }
        info(&report.note);

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze dataflow failed: {msg}"));
        process::exit(1);
    }
}

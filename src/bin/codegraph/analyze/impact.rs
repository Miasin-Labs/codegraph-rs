use super::*;

/// codegraph analyze impact <symbol> [--signature <sig>]
pub(crate) fn cmd_analyze_impact(
    symbol: &str,
    signature: Option<&str>,
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
        let Some(report) = analysis_reports::impact_report(&bridged.graph, &target, signature)
        else {
            info(&format!(
                "Symbol \"{symbol}\" not found in the analysis graph"
            ));
            return Ok(());
        };

        if json {
            return print_report_json("impact", &report);
        }

        if report.tasks.is_empty() {
            info(&format!(
                "No call sites found for \"{symbol}\" {} a signature edit cascades nowhere",
                get_glyphs().dash
            ));
            return Ok(());
        }

        println!(
            "{}",
            bold(&format!(
                "\nSignature-edit cascade for \"{symbol}\" {} {} call site{} in {} file{}:\n",
                get_glyphs().dash,
                report.call_site_count,
                if report.call_site_count == 1 { "" } else { "s" },
                report.task_count,
                if report.task_count == 1 { "" } else { "s" }
            ))
        );
        println!(
            "{}",
            dim(&format!("New signature: {}", report.new_signature))
        );
        println!();
        for task in &report.tasks {
            println!("{}", cyan(&task.file));
            for site in &task.call_sites {
                let loc = if site.line != 0 {
                    format!(":{}", site.line)
                } else {
                    String::new()
                };
                println!("  {}{}", white(&site.caller), dim(&loc));
            }
            println!();
        }
        info(
            "Cascade lists the direct call sites a signature edit must update; for the transitive blast radius use \"codegraph impact\".",
        );

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze impact failed: {msg}"));
        process::exit(1);
    }
}

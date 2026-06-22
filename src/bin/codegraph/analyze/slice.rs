use super::*;

/// codegraph analyze slice <symbol> [--direction fwd|bwd]
#[allow(clippy::too_many_arguments)]
pub(crate) fn cmd_analyze_slice(
    symbol: &str,
    direction_arg: &str,
    path_arg: Option<&str>,
    depth_arg: &str,
    value_level: bool,
    source_annotated: bool,
    no_cache: bool,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let direction = match direction_arg {
        "fwd" | "forward" => SliceDirection::Forward,
        "bwd" | "backward" => SliceDirection::Backward,
        other => {
            error_msg(&format!(
                "--direction must be \"fwd\" or \"bwd\" (got \"{other}\")."
            ));
            process::exit(1);
        }
    };
    if source_annotated && value_level {
        error_msg(
            "--source and --value-level are mutually exclusive: the --source report already \
             rides the engine's value-level oracle when the index carries byte offsets.",
        );
        process::exit(1);
    }

    if source_annotated {
        let body = || -> Result<(), String> {
            let bridged = bridge_project(&project_path, no_cache, json)?;
            let report = analysis_reports::source_slice_report(
                &bridged.graph,
                &project_path,
                symbol,
                direction,
                SOURCE_REPORT_MAX_ENTRIES,
            );
            if json {
                return print_report_json("sliceSource", &report);
            }
            println!();
            println!("{}", report.report.trim_end());
            println!();
            println!("{}", report.data_dependencies.trim_end());
            println!();
            warn(&report.note);
            Ok(())
        };
        if let Err(msg) = body() {
            error_msg(&format!("analyze slice failed: {msg}"));
            process::exit(1);
        }
        return;
    }

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let Some(seed) = resolve_symbol_via_index(&project_path, &bridged.id_map, symbol)? else {
            info(&format!(
                "Symbol \"{symbol}\" not found in the analysis graph"
            ));
            return Ok(());
        };
        let depth = parse_int_js(depth_arg).unwrap_or(10).clamp(1, 100) as usize;
        let report = if value_level {
            analyze_ir::value_slice_report(&bridged.graph, &project_path, &seed, direction, depth)
        } else {
            analysis_reports::slice_report(&bridged.graph, &seed, direction, depth)
        };
        let Some(report) = report else {
            info(&format!(
                "Symbol \"{symbol}\" not found in the analysis graph"
            ));
            return Ok(());
        };

        if json {
            return print_report_json("slice", &report);
        }

        if report.nodes.is_empty() {
            if value_level {
                // The value-level note explains *why* the slice is empty
                // (no value flow, fallback, or coverage gaps) — print it
                // instead of the generic no-call-edges line.
                info(&format!("Slice from \"{symbol}\" is empty"));
                warn(&report.note);
            } else {
                info(&format!(
                    "Slice from \"{symbol}\" is empty {} no call edges in that direction",
                    get_glyphs().dash
                ));
            }
            return Ok(());
        }

        let heading = match direction {
            SliceDirection::Forward => "Forward slice",
            SliceDirection::Backward => "Backward slice",
        };
        let granularity = if report.granularity == "value-level" {
            "value-level hops"
        } else {
            "call hops"
        };
        println!(
            "{}",
            bold(&format!(
                "\n{heading} of \"{symbol}\" ({} symbols within {} {granularity}):\n",
                report.size, report.max_depth
            ))
        );
        for node in &report.nodes {
            print_symbol_line(&node.kind, &node.name, &node.file, node.line);
            println!();
        }
        warn(&report.note);

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze slice failed: {msg}"));
        process::exit(1);
    }
}

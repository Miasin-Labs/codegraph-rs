use super::{
    analysis_reports,
    bold,
    bridge_project,
    dim,
    error_msg,
    get_glyphs,
    info,
    parse_int_js,
    print_report_json,
    print_symbol_line,
    process,
    resolve_project_path,
    resolve_symbol_via_index,
};

/// codegraph analyze dominators <symbol>
pub(crate) fn cmd_analyze_dominators(
    symbol: &str,
    path_arg: Option<&str>,
    top_arg: &str,
    no_cache: bool,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let Some(entry) = resolve_symbol_via_index(&project_path, &bridged.id_map, symbol)? else {
            info(&format!(
                "Symbol \"{symbol}\" not found in the analysis graph"
            ));
            return Ok(());
        };
        let top = parse_int_js(top_arg).unwrap_or(50).max(1) as usize;
        let Some(report) = analysis_reports::dominators_report(&bridged.graph, &entry, top) else {
            info(&format!(
                "Symbol \"{symbol}\" not found in the analysis graph"
            ));
            return Ok(());
        };

        if json {
            return print_report_json("dominators", &report);
        }

        if report.nodes.is_empty() {
            info(&format!("No nodes reachable from \"{symbol}\""));
            return Ok(());
        }

        println!(
            "{}",
            bold(&format!(
                "\nDominators from \"{symbol}\" ({} reachable nodes analyzed):\n",
                report.analyzed
            ))
        );
        for entry in &report.nodes {
            print_symbol_line(
                &entry.symbol.kind,
                &entry.symbol.name,
                &entry.symbol.file,
                entry.symbol.line,
            );
            if let Some(idom) = &entry.immediate_dominator {
                println!(
                    "{}",
                    dim(&format!(
                        "  immediate dominator: {} (chain depth {})",
                        idom.name, entry.dominator_depth
                    ))
                );
            }
            println!();
        }
        if report.truncated {
            info(&format!(
                "Output capped {} raise with --top to analyze more nodes",
                get_glyphs().dash
            ));
        }

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze dominators failed: {msg}"));
        process::exit(1);
    }
}

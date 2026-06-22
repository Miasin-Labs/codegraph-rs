use super::*;

/// codegraph analyze centrality [--top N]
pub(crate) fn cmd_analyze_centrality(
    top_arg: &str,
    path_arg: Option<&str>,
    no_cache: bool,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let top = parse_int_js(top_arg).unwrap_or(20).max(1) as usize;
        let report = analysis_reports::centrality_report(&bridged.graph, top);

        if json {
            return print_report_json("centrality", &report);
        }

        if report.nodes.is_empty() {
            info("No symbols to rank (empty graph)");
            return Ok(());
        }

        println!(
            "{}",
            bold(&format!(
                "\nMost central symbols (PageRank over {} nodes, damping {}):\n",
                format_number(report.analyzed as u64),
                js_to_fixed(report.damping_factor, 2)
            ))
        );
        for ranked in &report.nodes {
            print_symbol_line(
                &ranked.symbol.kind,
                &ranked.symbol.name,
                &ranked.symbol.file,
                ranked.symbol.line,
            );
            println!(
                "{}",
                dim(&format!("  score {}", js_to_fixed(ranked.score, 4)))
            );
            println!();
        }

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze centrality failed: {msg}"));
        process::exit(1);
    }
}

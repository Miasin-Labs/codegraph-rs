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
    success,
    white,
};

/// codegraph analyze critical
pub(crate) fn cmd_analyze_critical(
    top_arg: &str,
    path_arg: Option<&str>,
    no_cache: bool,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let top = parse_int_js(top_arg).unwrap_or(25).max(1) as usize;
        let report = analysis_reports::critical_report(&bridged.graph, top);

        if json {
            return print_report_json("critical", &report);
        }

        if report.nodes.is_empty() && report.bridges.is_empty() {
            success("No articulation nodes or bridge edges — no single point of failure");
            return Ok(());
        }

        if !report.nodes.is_empty() {
            println!(
                "{}",
                bold(&format!(
                    "\nArticulation nodes ({}) — removal disconnects the graph:\n",
                    report.articulation_count
                ))
            );
            for node in &report.nodes {
                print_symbol_line(&node.kind, &node.name, &node.file, node.line);
                println!();
            }
        }
        if !report.bridges.is_empty() {
            println!(
                "{}",
                bold(&format!("Bridge edges ({}):\n", report.bridge_count))
            );
            for bridge in &report.bridges {
                println!(
                    "  {} {} {}",
                    white(&bridge.from.name),
                    dim("->"),
                    white(&bridge.to.name)
                );
            }
            println!();
        }
        if report.truncated {
            info(&format!(
                "Output capped {} raise with --top for more",
                get_glyphs().dash
            ));
        }
        info(&report.note);

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze critical failed: {msg}"));
        process::exit(1);
    }
}

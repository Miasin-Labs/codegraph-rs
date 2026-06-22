use super::*;

/// codegraph analyze stats [--estimate-reachability]
pub(crate) fn cmd_analyze_stats(
    estimate_reachability: bool,
    top_arg: &str,
    path_arg: Option<&str>,
    no_cache: bool,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let top = parse_int_js(top_arg).unwrap_or(10).max(1) as usize;
        let report = analysis_reports::stats_report(&bridged.graph, estimate_reachability, top);

        if json {
            return print_report_json("stats", &report);
        }

        println!("{}", bold("\nBridged analysis graph:\n"));
        let nodes_by_kind: Vec<String> = report
            .nodes_by_kind
            .iter()
            .map(|(kind, count)| format!("{kind} {}", format_number(*count as u64)))
            .collect();
        let edges_by_kind: Vec<String> = report
            .edges_by_kind
            .iter()
            .map(|(kind, count)| format!("{kind} {}", format_number(*count as u64)))
            .collect();
        println!(
            "  {} {}",
            white(&format!(
                "{} nodes",
                format_number(report.node_count as u64)
            )),
            dim(&format!("({})", nodes_by_kind.join(", ")))
        );
        println!(
            "  {} {}",
            white(&format!(
                "{} edges",
                format_number(report.edge_count as u64)
            )),
            dim(&format!("({})", edges_by_kind.join(", ")))
        );
        println!(
            "  {} {}",
            white(&format!(
                "{} files",
                format_number(report.file_count as u64)
            )),
            dim(&format!(
                "{} unresolved-call placeholders",
                format_number(report.placeholder_count as u64)
            ))
        );
        println!();

        if let Some(reachability) = &report.reachability {
            println!(
                "{}",
                bold(&format!(
                    "Widest-reaching symbols ({}):\n",
                    reachability.method
                ))
            );
            for entry in &reachability.top {
                println!(
                    "  {} {}",
                    white(&entry.symbol.name),
                    dim(&format!(
                        "reaches {}, reached by {} ({})",
                        js_to_fixed(entry.descendants, 0),
                        js_to_fixed(entry.ancestors, 0),
                        entry.symbol.file
                    ))
                );
            }
            println!();
            info(&reachability.note);
        }

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze stats failed: {msg}"));
        process::exit(1);
    }
}

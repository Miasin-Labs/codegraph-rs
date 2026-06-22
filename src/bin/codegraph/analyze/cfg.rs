use super::*;

/// codegraph analyze cfg <symbol>
///
/// Control-flow graph of one function, built by re-parsing its on-disk
/// source with the host grammars (the `analyze complexity` anchor pattern).
/// Languages without engine CFG rules get the report's honest capability
/// note instead of an empty graph.
pub(crate) fn cmd_analyze_cfg(symbol: &str, path_arg: Option<&str>, no_cache: bool, json: bool) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let Some(target) = resolve_symbol_via_index(&project_path, &bridged.id_map, symbol)? else {
            info(&format!(
                "Symbol \"{symbol}\" not found in the analysis graph"
            ));
            return Ok(());
        };
        let Some(report) = analyze_ir::cfg_report(&bridged.graph, &project_path, &target) else {
            info(&format!(
                "Symbol \"{symbol}\" not found in the analysis graph"
            ));
            return Ok(());
        };

        if json {
            return print_report_json("cfg", &report);
        }

        if !report.analyzed {
            // Honest capability note — never an empty block list.
            warn(&report.note);
            return Ok(());
        }

        println!(
            "{}",
            bold(&format!(
                "\nControl-flow graph of \"{}\" ({} block{}, {} edge{}):\n",
                report.symbol.name,
                report.block_count,
                if report.block_count == 1 { "" } else { "s" },
                report.edge_count,
                if report.edge_count == 1 { "" } else { "s" },
            ))
        );
        print_symbol_line(
            &report.symbol.kind,
            &report.symbol.name,
            &report.symbol.file,
            report.symbol.line,
        );
        println!();
        println!("{}", cyan("Blocks:"));
        for block in &report.blocks {
            let lines = if block.start_line == 0 && block.end_line == 0 {
                String::new()
            } else if block.start_line == block.end_line {
                format!("  line {}", block.start_line)
            } else {
                format!("  lines {}-{}", block.start_line, block.end_line)
            };
            println!(
                "  {} {}{}",
                white(&format!("[{}] {}", block.id, block.label)),
                dim(&format!("({})", block.kind)),
                dim(&lines)
            );
        }
        println!();
        println!("{}", cyan("Edges:"));
        for edge in &report.edges {
            println!(
                "  {} {} {} {}",
                white(&edge.from.to_string()),
                dim("->"),
                white(&edge.to.to_string()),
                dim(&format!("({})", edge.kind))
            );
        }
        println!();
        info(&report.note);

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze cfg failed: {msg}"));
        process::exit(1);
    }
}

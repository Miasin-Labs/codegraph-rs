use super::*;

/// codegraph analyze cycles
pub(crate) fn cmd_analyze_cycles(path_arg: Option<&str>, no_cache: bool, json: bool) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let report = analysis_reports::cycles_report(&bridged.graph);

        if json {
            return print_report_json("cycles", &report);
        }

        if report.cycles.is_empty() {
            success("No dependency cycles or recursion clusters found");
            return Ok(());
        }

        println!(
            "{}",
            bold(&format!(
                "\nDependency cycles and recursion clusters ({}):\n",
                report.cycle_count
            ))
        );
        for cycle in &report.cycles {
            println!(
                "{} {}",
                cyan(cycle_kind_label(&cycle.kind)),
                dim(&format!(
                    "({} member{})",
                    cycle.size,
                    if cycle.size == 1 { "" } else { "s" }
                ))
            );
            for member in &cycle.members {
                let loc = if member.line != 0 {
                    format!(":{}", member.line)
                } else {
                    String::new()
                };
                println!(
                    "  {} {}",
                    white(&member.name),
                    dim(&format!("{}{loc}", member.file))
                );
            }
            println!();
        }
        for suggestion in &report.break_suggestions {
            info(&format!(
                "Break suggestion: remove the {} -> {} edge",
                suggestion.from.name, suggestion.to.name
            ));
        }

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze cycles failed: {msg}"));
        process::exit(1);
    }
}

use super::*;

/// codegraph analyze capabilities
///
/// Pure environment read — works without an initialized project.
pub(crate) fn cmd_analyze_capabilities(json: bool) {
    let report = analysis_reports::capabilities_report();

    if json {
        if let Err(msg) = print_report_json("capabilities", &report) {
            error_msg(&format!("analyze capabilities failed: {msg}"));
            process::exit(1);
        }
        return;
    }

    println!("{}", bold("\nAnalysis-engine capabilities:\n"));
    for capability in &report.capabilities {
        let state = if capability.enabled {
            green("on ")
        } else {
            red("off")
        };
        println!(
            "  {} {} {}",
            state,
            white(&format!("{:<18}", capability.name)),
            dim(&capability.env_var)
        );
        if let Some(value) = &capability.env_value {
            println!("{}", dim(&format!("      env override: \"{value}\"")));
        }
        if !capability.disables.is_empty() {
            println!(
                "{}",
                dim(&format!(
                    "      disabling also disables: {}",
                    capability.disables.join(", ")
                ))
            );
        }
    }
    println!();
    info(&report.note);
}

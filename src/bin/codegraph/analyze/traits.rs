use super::*;

/// codegraph analyze traits [type]
pub(crate) fn cmd_analyze_traits(
    type_name: Option<&str>,
    path_arg: Option<&str>,
    no_cache: bool,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let report = analysis_reports::traits_report(&bridged.graph, type_name);

        if json {
            return print_report_json("traits", &report);
        }

        if report.hierarchies.is_empty() && report.clusters.is_empty() {
            match type_name {
                Some(filter) => info(&format!(
                    "No trait hierarchy or type cluster matches \"{filter}\""
                )),
                None => info(&report.note),
            }
            return Ok(());
        }

        if !report.hierarchies.is_empty() {
            println!(
                "{}",
                bold(&format!("\nTrait hierarchies ({}):\n", report.trait_count))
            );
            for hierarchy in &report.hierarchies {
                println!(
                    "{} {}",
                    cyan(&hierarchy.trait_ref.name),
                    dim(&format!(
                        "({} implementor{}, {})",
                        hierarchy.implementor_count,
                        if hierarchy.implementor_count == 1 {
                            ""
                        } else {
                            "s"
                        },
                        hierarchy.trait_ref.file
                    ))
                );
                for implementor in &hierarchy.implementors {
                    println!("  {} {}", white(&implementor.name), dim(&implementor.file));
                }
                println!();
            }
        }

        if !report.dispatch_calls.is_empty() {
            println!(
                "{}",
                bold(&format!(
                    "Trait-dispatch calls ({}):\n",
                    report.dispatch_call_count
                ))
            );
            for dispatch in &report.dispatch_calls {
                println!(
                    "  {} {} {} {}",
                    white(&dispatch.caller.name),
                    dim("->"),
                    white(&dispatch.callee.name),
                    dim(&format!("(via {})", dispatch.trait_ref.name))
                );
            }
            println!();
        }

        if !report.clusters.is_empty() {
            println!(
                "{}",
                bold(&format!(
                    "Functions clustered by primary type ({}):\n",
                    report.cluster_count
                ))
            );
            for cluster in &report.clusters {
                let names: Vec<&str> = cluster.functions.iter().map(|f| f.name.as_str()).collect();
                let more = if cluster.truncated {
                    format!(
                        " (+{} more)",
                        cluster.function_count - cluster.functions.len()
                    )
                } else {
                    String::new()
                };
                println!(
                    "{} {}",
                    cyan(&cluster.primary_type.name),
                    dim(&format!("({} functions)", cluster.function_count))
                );
                println!("  {}{}", names.join(", "), dim(&more));
                println!();
            }
        }
        info(&report.note);

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze traits failed: {msg}"));
        process::exit(1);
    }
}

use super::*;

/// codegraph analyze query "<dsl>" [--why] [--explain] [--lcov <path>]
///
/// Runs the analysis engine's pipe-based query DSL over the bridged graph.
/// `--explain` parses + optimises only (never touches the index, so it works
/// without an initialized project); `--why` adds per-row provenance;
/// `--lcov` annotates coverage onto the in-memory graph first so the
/// `untested` operator returns real rows instead of treating every function
/// as untested.
#[allow(clippy::too_many_arguments)]
pub(crate) fn cmd_analyze_query(
    query: &str,
    path_arg: Option<&str>,
    max_nodes_arg: &str,
    why: bool,
    explain: bool,
    lcov: Option<&str>,
    no_cache: bool,
    json: bool,
) {
    let body = || -> Result<(), String> {
        if explain {
            let report = analysis_reports::explain_report(query)?;
            if json {
                return print_report_json("queryPlan", &report);
            }
            println!(
                "{}",
                bold(&format!(
                    "\nOptimised plan ({} query, not executed):\n",
                    report.kind
                ))
            );
            for (i, step) in report.steps.iter().enumerate() {
                println!("  {} {}", dim(&format!("{}.", i + 1)), white(step));
            }
            println!();
            info(&format!(
                "BFS schedule hint: {}{}",
                report.strategy,
                if report.parallel { " (parallel)" } else { "" }
            ));
            return Ok(());
        }

        let project_path = resolve_project_path(path_arg);
        let mut bridged = bridge_project(&project_path, no_cache, json)?;
        if let Some(lcov_path) = lcov {
            analysis_reports::annotate_coverage(
                &mut bridged.graph,
                Path::new(lcov_path),
                &project_path,
            )?;
        }
        let max_nodes = parse_int_js(max_nodes_arg).unwrap_or(50).max(1) as usize;
        let report = analysis_reports::query_report_with_sources(
            &bridged.graph,
            query,
            max_nodes,
            why,
            Some(&project_path),
        )?;

        if json {
            return print_report_json("query", &report);
        }

        if report.nodes.is_empty() && report.metadata.is_empty() {
            info("Query matched no nodes");
            return Ok(());
        }

        if !report.nodes.is_empty() {
            println!(
                "{}",
                bold(&format!(
                    "\nQuery results ({} node{}{}):\n",
                    report.node_count,
                    if report.node_count == 1 { "" } else { "s" },
                    if report.truncated {
                        format!(" of {}", report.total_before_truncation)
                    } else {
                        String::new()
                    }
                ))
            );
            let kind_w = report
                .nodes
                .iter()
                .map(|n| n.kind.len())
                .chain(["KIND".len()])
                .max()
                .unwrap_or(4);
            let name_w = report
                .nodes
                .iter()
                .map(|n| n.name.len())
                .chain(["NAME".len()])
                .max()
                .unwrap_or(4);
            println!(
                "  {}  {}  {}",
                dim(&format!("{:<kind_w$}", "KIND")),
                dim(&format!("{:<name_w$}", "NAME")),
                dim("LOCATION")
            );
            for row in &report.nodes {
                let loc = if row.line != 0 {
                    format!("{}:{}", row.file, row.line)
                } else {
                    row.file.clone()
                };
                println!(
                    "  {}  {}  {}",
                    cyan(&format!("{:<kind_w$}", row.kind)),
                    white(&format!("{:<name_w$}", row.name)),
                    dim(&loc)
                );
            }
            println!();
        }

        if !report.edges.is_empty() {
            const EDGE_CAP: usize = 25;
            println!("{}", bold("Edges:"));
            for edge in report.edges.iter().take(EDGE_CAP) {
                println!(
                    "  {} {} {} {}",
                    white(&edge.from),
                    dim("->"),
                    white(&edge.to),
                    dim(&format!("({})", edge.kind))
                );
            }
            if report.edges.len() > EDGE_CAP {
                println!(
                    "{}",
                    dim(&format!(
                        "  ... {} more (use --json for all)",
                        report.edges.len() - EDGE_CAP
                    ))
                );
            }
            println!();
        }

        if !report.metadata.is_empty() {
            for line in &report.metadata {
                println!("{}", dim(line));
            }
            println!();
        }

        if let Some(pre) = &report.preconditions {
            if !pre.guards.is_empty() {
                println!("{}", bold("Guarding conditions (source-level):"));
                for guard in &pre.guards {
                    println!(
                        "  {} {} {} {}",
                        white(&guard.caller.name),
                        dim("->"),
                        white(&guard.callee),
                        dim(&format!("({}:{})", guard.file, guard.line))
                    );
                    println!("    {}", cyan(&guard.conditions.join(" -> ")));
                }
                println!();
            }
            info(&pre.note);
        }

        if why {
            match &report.why {
                Some(entries) if !entries.is_empty() => {
                    println!("{}", bold("Why (provenance):"));
                    for entry in entries {
                        for step in &entry.steps {
                            let origin = if step.predecessors.is_empty() {
                                "seed".to_string()
                            } else {
                                format!("from {}", step.predecessors.join(", "))
                            };
                            println!(
                                "  {} {}",
                                white(&entry.symbol.name),
                                dim(&format!("<- {} ({origin}, stage {})", step.op, step.stage))
                            );
                        }
                    }
                    println!();
                }
                Some(_) => {}
                None => info("why-provenance is not available for aggregation queries"),
            }
        }

        if report.truncated {
            info(&format!(
                "Result truncated to {} nodes {} raise --max-nodes for more",
                report.node_count,
                get_glyphs().dash
            ));
        }

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze query failed: {msg}"));
        process::exit(1);
    }
}

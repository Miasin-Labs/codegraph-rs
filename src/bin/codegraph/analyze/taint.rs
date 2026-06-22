use super::*;

/// codegraph analyze taint [source-symbol] [sink-symbol] [--suggest]
///
/// With both symbols: call-graph paths from source to sink (the existing
/// behavior). With `--suggest` — or when both symbols are omitted — ranks
/// candidate sources/sinks by identifier naming instead.
#[allow(clippy::too_many_arguments)]
pub(crate) fn cmd_analyze_taint(
    source: Option<&str>,
    sink: Option<&str>,
    suggest: bool,
    value_level: bool,
    source_annotated: bool,
    path_arg: Option<&str>,
    max_nodes_arg: &str,
    top_arg: &str,
    no_cache: bool,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    if source_annotated && value_level {
        error_msg(
            "--source and --value-level are mutually exclusive: the --source report already \
             rides the engine's value-level oracle when the index carries byte offsets.",
        );
        process::exit(1);
    }

    if suggest || (source.is_none() && sink.is_none()) {
        if value_level {
            error_msg(
                "--value-level applies to source\u{2192}sink tracing; give both \
                 <source-symbol> and <sink-symbol> (it has no effect on --suggest).",
            );
            process::exit(1);
        }
        if source_annotated {
            error_msg(
                "--source applies to source\u{2192}sink tracing; give both <source-symbol> \
                 and <sink-symbol> (it has no effect on --suggest).",
            );
            process::exit(1);
        }
        let body = || -> Result<(), String> {
            let bridged = bridge_project(&project_path, no_cache, json)?;
            let top = parse_int_js(top_arg).unwrap_or(20).max(1) as usize;
            let report = analysis_reports::taint_suggest_report(&bridged.graph, top);

            if json {
                return print_report_json("taintSuggest", &report);
            }

            if report.source_count == 0 && report.sink_count == 0 {
                info(&report.note);
                return Ok(());
            }

            println!(
                "{}",
                bold(&format!(
                    "\nSuggested taint candidates ({} source{}, {} sink{} of {} functions):\n",
                    report.source_count,
                    if report.source_count == 1 { "" } else { "s" },
                    report.sink_count,
                    if report.sink_count == 1 { "" } else { "s" },
                    format_number(report.functions_classified as u64)
                ))
            );
            if !report.sources.is_empty() {
                println!("{}", cyan("Sources (named like untrusted input):"));
                for candidate in &report.sources {
                    println!(
                        "  {} {}",
                        white(&candidate.symbol.name),
                        dim(&format!(
                            "{} (score {})",
                            candidate.symbol.file,
                            js_to_fixed(candidate.score, 2)
                        ))
                    );
                }
                println!();
            }
            if !report.sinks.is_empty() {
                println!("{}", cyan("Sinks (named like dangerous operations):"));
                for candidate in &report.sinks {
                    println!(
                        "  {} {}",
                        white(&candidate.symbol.name),
                        dim(&format!(
                            "{} (score {})",
                            candidate.symbol.file,
                            js_to_fixed(candidate.score, 2)
                        ))
                    );
                }
                println!();
            }
            if !report.pairs.is_empty() {
                println!("{}", cyan("Top pairs to confirm:"));
                for pair in &report.pairs {
                    println!(
                        "  {} {} {} {}",
                        white(&pair.source.name),
                        dim("->"),
                        white(&pair.sink.name),
                        dim(&format!("(priority {})", js_to_fixed(pair.priority, 2)))
                    );
                }
                println!();
            }
            info(&report.note);
            Ok(())
        };
        if let Err(msg) = body() {
            error_msg(&format!("analyze taint failed: {msg}"));
            process::exit(1);
        }
        return;
    }

    let (Some(source), Some(sink)) = (source, sink) else {
        error_msg(
            "analyze taint needs both <source-symbol> and <sink-symbol> (or --suggest / no \
             symbols for name-based suggestion).",
        );
        process::exit(1);
    };

    if source_annotated {
        let body = || -> Result<(), String> {
            let bridged = bridge_project(&project_path, no_cache, json)?;
            let report = analysis_reports::source_taint_report(
                &bridged.graph,
                &project_path,
                source,
                sink,
                SOURCE_TAINT_MAX_PATHS,
            );
            if json {
                return print_report_json("taintSource", &report);
            }
            println!();
            println!("{}", report.report.trim_end());
            println!();
            warn(&report.note);
            Ok(())
        };
        if let Err(msg) = body() {
            error_msg(&format!("analyze taint failed: {msg}"));
            process::exit(1);
        }
        return;
    }

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let cg =
            CodeGraph::open(&project_path, &OpenOptions::default()).map_err(|e| e.to_string())?;
        let source_id = resolve_analysis_symbol(&cg, &bridged.id_map, source)?;
        let sink_id = resolve_analysis_symbol(&cg, &bridged.id_map, sink)?;
        cg.close();
        let Some(source_id) = source_id else {
            info(&format!(
                "Symbol \"{source}\" not found in the analysis graph"
            ));
            return Ok(());
        };
        let Some(sink_id) = sink_id else {
            info(&format!(
                "Symbol \"{sink}\" not found in the analysis graph"
            ));
            return Ok(());
        };

        let max_nodes = parse_int_js(max_nodes_arg).unwrap_or(6).clamp(0, 32) as usize;
        let report = if value_level {
            analyze_ir::value_taint_report(
                &bridged.graph,
                &project_path,
                &source_id,
                &sink_id,
                max_nodes,
                25,
            )
        } else {
            analysis_reports::taint_report(&bridged.graph, &source_id, &sink_id, max_nodes, 25)
        };
        let Some(report) = report else {
            info("Source or sink not found in the analysis graph");
            return Ok(());
        };

        if json {
            return print_report_json("taint", &report);
        }

        if report.paths.is_empty() {
            if value_level {
                // The value-level note explains the absence (no value flow,
                // call-graph fallback, or coverage gaps) — print it instead
                // of the generic intermediate-node-cap line.
                if report.granularity == "value-level" {
                    info(&format!(
                        "No value-level flow from \"{source}\" to \"{sink}\""
                    ));
                } else {
                    info(&format!("No paths from \"{source}\" to \"{sink}\""));
                }
                warn(&report.note);
            } else {
                info(&format!(
                    "No paths from \"{source}\" to \"{sink}\" within {} intermediate nodes",
                    report.max_intermediate_nodes
                ));
            }
            return Ok(());
        }

        println!(
            "{}",
            bold(&format!(
                "\nPaths from \"{source}\" to \"{sink}\" ({} found{}):\n",
                report.path_count,
                if report.truncated {
                    format!(", showing {}", report.paths.len())
                } else {
                    String::new()
                }
            ))
        );
        for path in &report.paths {
            let names: Vec<&str> = path.nodes.iter().map(|n| n.name.as_str()).collect();
            println!("  {}", white(&names.join(" -> ")));
            println!(
                "{}",
                dim(&format!("    via {}", path.edge_kinds.join(", ")))
            );
            println!();
        }
        warn(&report.note);

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze taint failed: {msg}"));
        process::exit(1);
    }
}

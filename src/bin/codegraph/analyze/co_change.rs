use super::*;

/// codegraph analyze co-change [symbol] [--min-support N] [--max-commits N]
pub(crate) fn cmd_analyze_co_change(
    symbol: Option<&str>,
    min_support_arg: &str,
    max_commits_arg: &str,
    top_arg: &str,
    path_arg: Option<&str>,
    no_cache: bool,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let seed = match symbol {
            Some(symbol) => {
                let Some(seed) = resolve_symbol_via_index(&project_path, &bridged.id_map, symbol)?
                else {
                    info(&format!(
                        "Symbol \"{symbol}\" not found in the analysis graph"
                    ));
                    return Ok(());
                };
                Some(seed)
            }
            None => None,
        };
        let min_support = parse_int_js(min_support_arg).unwrap_or(2).max(1) as u32;
        let max_commits = parse_int_js(max_commits_arg).unwrap_or(500).max(1) as usize;
        let top = parse_int_js(top_arg).unwrap_or(25).max(1) as usize;
        let report = analysis_reports::co_change_report(
            &bridged.graph,
            &project_path,
            seed.as_ref(),
            min_support,
            max_commits,
            top,
        );

        if json {
            return print_report_json("coChange", &report);
        }

        if report.commits_analyzed == 0 {
            warn(&report.note);
            return Ok(());
        }
        if report.pairs.is_empty() {
            info(&format!(
                "No cross-file co-change pairs at min support {} across {} commits{}",
                report.min_support,
                format_number(report.commits_analyzed as u64),
                symbol
                    .map(|s| format!(" touching \"{s}\""))
                    .unwrap_or_default()
            ));
            return Ok(());
        }

        println!(
            "{}",
            bold(&format!(
                "\nCo-change pairs ({} of {} cross-file, {} commits, min support {}):\n",
                report.pairs.len(),
                format_number(report.cross_file_pair_count as u64),
                format_number(report.commits_analyzed as u64),
                report.min_support
            ))
        );
        for pair in &report.pairs {
            println!(
                "  {} {} {}",
                white(&pair.a.name),
                dim("<->"),
                white(&pair.b.name)
            );
            println!(
                "{}",
                dim(&format!(
                    "    together {}x, confidence {} ({} <-> {})",
                    pair.times_changed_together,
                    js_to_fixed(pair.confidence, 2),
                    pair.a.file,
                    pair.b.file
                ))
            );
        }
        println!();
        if report.same_file_pair_count > 0 {
            info(&format!(
                "{} same-file pairs folded (same-file symbols co-change by construction)",
                format_number(report.same_file_pair_count as u64)
            ));
        }

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze co-change failed: {msg}"));
        process::exit(1);
    }
}

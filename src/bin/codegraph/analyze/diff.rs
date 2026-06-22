use super::{
    DatabaseConnection,
    Path,
    QueryBuilder,
    analysis_reports,
    bold,
    build_analysis_graph_cached,
    compute_index_fingerprint,
    cyan,
    cycle_kind_label,
    dim,
    error_msg,
    get_database_path,
    get_glyphs,
    green,
    info,
    is_initialized,
    load_auto_base_snapshot,
    load_explicit_base_snapshot,
    parse_int_js,
    print_report_json,
    process,
    red,
    resolve_project_path,
    store_complexity_sidecar,
    success,
    white,
    yellow,
};

/// The honest no-base note `analyze diff` prints (exit 0): a diff needs a
/// snapshot of the pre-edit state, and any analyze command caches one.
const NO_BASE_NOTE: &str = "no base snapshot — run any analyze command on the base state first";

/// codegraph analyze diff [--base <snapshot|auto>] [--depth N] [--top N]
///
/// Working-tree vs base. Bridges the current index FIRST (which refreshes
/// the snapshot cache — rotation preserves the pre-edit generation as
/// `.prev`), then resolves the base: `auto` picks the last cached snapshot
/// built from a different index fingerprint (stale current generation, else
/// `.prev`); an explicit path loads a snapshot file or cache directory.
/// After diffing, the working tree's per-function complexity is written as
/// a sidecar next to the current snapshot so the NEXT diff has before
/// metrics.
pub(crate) fn cmd_analyze_diff(
    base_arg: &str,
    depth_arg: &str,
    top_arg: &str,
    path_arg: Option<&str>,
    no_cache: bool,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        // Same init/open contract as `bridge_project`, but the fingerprint
        // is needed for base resolution, so the cache wrapper is driven
        // directly here.
        if !is_initialized(&project_path) {
            error_msg(&format!(
                "CodeGraph not initialized in {}",
                project_path.display()
            ));
            info("Run \"codegraph init\" first");
            process::exit(1);
        }
        let conn = DatabaseConnection::open(get_database_path(&project_path))
            .map_err(|e| e.to_string())?;
        let queries = QueryBuilder::new(conn.get_db().map_err(|e| e.to_string())?);
        let fingerprint = compute_index_fingerprint(&queries).map_err(|e| e.to_string())?;
        let cached = build_analysis_graph_cached(&queries, &project_path, !no_cache)
            .map_err(|e| e.to_string())?;
        if cached.from_cache && !json {
            println!("{}", dim("(cached graph)"));
        }
        let bridged = cached.result;

        let base = if base_arg == "auto" {
            load_auto_base_snapshot(&project_path, fingerprint)
        } else {
            Some(
                load_explicit_base_snapshot(Path::new(base_arg))
                    .map_err(|e| format!("cannot load base snapshot \"{base_arg}\": {e}"))?,
            )
        };
        let Some(base) = base else {
            // Honest no-base case (exit 0): nothing older than the working
            // tree is cached. This run primed the cache, so after the next
            // edit + re-index a plain `analyze diff` will work.
            if json {
                return print_report_json(
                    "diff",
                    &serde_json::json!({ "baseAvailable": false, "note": NO_BASE_NOTE }),
                );
            }
            info(NO_BASE_NOTE);
            return Ok(());
        };

        let depth = parse_int_js(depth_arg).unwrap_or(3).max(1) as usize;
        let top = parse_int_js(top_arg).unwrap_or(50).max(1) as usize;
        let current_complexity =
            analysis_reports::measure_complexity_map(&bridged.graph, &project_path);
        let report =
            analysis_reports::diff_report(&base, &bridged.graph, &current_complexity, depth, top);
        // Best-effort: annotate the current generation so the next diff has
        // before-metrics. A failure only degrades that future report.
        let _ = store_complexity_sidecar(&project_path, fingerprint, &current_complexity);

        if json {
            return print_report_json("diff", &report);
        }

        let base_label = match &report.base.index_fingerprint {
            Some(fp) => format!("{} {fp}", report.base.source),
            None => report.base.source.clone(),
        };
        if report.nodes_added_count == 0
            && report.nodes_removed_count == 0
            && report.nodes_changed_count == 0
            && report.edges_added_count == 0
            && report.edges_removed_count == 0
        {
            success(&format!(
                "No differences vs the base snapshot ({base_label})"
            ));
            return Ok(());
        }

        println!(
            "{}",
            bold(&format!("\nDiff vs base snapshot ({base_label}):\n"))
        );

        println!(
            "{}",
            bold(&format!(
                "Nodes: {} added, {} removed, {} changed",
                report.nodes_added_count, report.nodes_removed_count, report.nodes_changed_count
            ))
        );
        let loc = |file: &str, line: u32| {
            if line != 0 {
                format!("{file}:{line}")
            } else {
                file.to_string()
            }
        };
        for n in &report.nodes_added {
            println!(
                "  {} {} {} {}",
                green("+"),
                cyan(&n.kind),
                white(&n.name),
                dim(&loc(&n.file, n.line))
            );
        }
        for n in &report.nodes_removed {
            println!(
                "  {} {} {} {}",
                red("-"),
                cyan(&n.kind),
                white(&n.name),
                dim(&loc(&n.file, n.line))
            );
        }
        for n in &report.nodes_changed {
            println!(
                "  {} {} {} {} {}",
                yellow("~"),
                cyan(&n.symbol.kind),
                white(&n.symbol.name),
                dim(&loc(&n.symbol.file, n.symbol.line)),
                dim(&format!("({})", n.reasons.join(", ")))
            );
        }
        println!();

        if report.edges_added_count > 0 || report.edges_removed_count > 0 {
            println!(
                "{}",
                bold(&format!(
                    "Edges: {} added, {} removed",
                    report.edges_added_count, report.edges_removed_count
                ))
            );
            for e in &report.edges_added {
                println!(
                    "  {} {} {} {} {}",
                    green("+"),
                    white(&e.from),
                    dim("->"),
                    white(&e.to),
                    dim(&format!("({})", e.kind))
                );
            }
            for e in &report.edges_removed {
                println!(
                    "  {} {} {} {} {}",
                    red("-"),
                    white(&e.from),
                    dim("->"),
                    white(&e.to),
                    dim(&format!("({})", e.kind))
                );
            }
            println!();
        }

        if !report.changed_functions.is_empty() {
            println!("{}", bold("Changed functions (complexity):"));
            let fmt_metric = |before: Option<u32>, after: Option<u32>, delta: Option<i64>| {
                let b = before.map_or("?".to_string(), |v| v.to_string());
                let a = after.map_or("?".to_string(), |v| v.to_string());
                match delta {
                    Some(d) => format!("{b} -> {a} ({d:+})"),
                    None => format!("{b} -> {a}"),
                }
            };
            for f in &report.changed_functions {
                println!(
                    "  {} {} {} {}",
                    white(&f.symbol.name),
                    dim(&format!(
                        "cyclomatic {}",
                        fmt_metric(f.cyclomatic_before, f.cyclomatic_after, f.cyclomatic_delta)
                    )),
                    dim(&format!(
                        "cognitive {}",
                        fmt_metric(f.cognitive_before, f.cognitive_after, f.cognitive_delta)
                    )),
                    dim(&format!("lines {} -> {}", f.lines_before, f.lines_after))
                );
            }
            println!();
        }

        if report.new_cycle_count > 0 {
            println!(
                "{}",
                bold(&format!(
                    "Newly-introduced cycles ({}):",
                    report.new_cycle_count
                ))
            );
            for cycle in &report.new_cycles {
                let members: Vec<&str> = cycle.members.iter().map(|m| m.name.as_str()).collect();
                println!(
                    "  {} {}",
                    cyan(cycle_kind_label(&cycle.kind)),
                    white(&members.join(", "))
                );
            }
            println!();
        }
        if report.resolved_cycle_count > 0 {
            info(&format!(
                "{} cycle{} from the base no longer exist{}",
                report.resolved_cycle_count,
                if report.resolved_cycle_count == 1 {
                    ""
                } else {
                    "s"
                },
                if report.resolved_cycle_count == 1 {
                    "s"
                } else {
                    ""
                }
            ));
        }

        println!(
            "{}",
            bold(&format!(
                "Impact of the delta (depth {}): {} symbol{}",
                report.impact.depth,
                report.impact.impacted_count,
                if report.impact.impacted_count == 1 {
                    ""
                } else {
                    "s"
                }
            ))
        );
        for n in &report.impact.nodes {
            println!(
                "  {} {} {}",
                cyan(&n.kind),
                white(&n.name),
                dim(&loc(&n.file, n.line))
            );
        }
        println!();

        if report.truncated || report.impact.truncated {
            info(&format!(
                "Listings capped at {top} entries {} raise --top for more (counts are exact)",
                get_glyphs().dash
            ));
        }
        info(&report.note);

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze diff failed: {msg}"));
        process::exit(1);
    }
}

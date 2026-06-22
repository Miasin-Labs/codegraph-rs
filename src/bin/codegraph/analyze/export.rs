use super::*;

/// codegraph analyze export --format dot [--symbol <s> --depth N]
///
/// Human output is the raw DOT document (pipe straight to `dot -Tsvg`);
/// `--json` wraps it in the envelope.
pub(crate) fn cmd_analyze_export(
    format: &str,
    symbol: Option<&str>,
    depth_arg: &str,
    path_arg: Option<&str>,
    no_cache: bool,
    json: bool,
) {
    if format != "dot" {
        error_msg(&format!(
            "--format must be \"dot\" (got \"{format}\"); other formats are not supported yet."
        ));
        process::exit(1);
    }
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        // The DOT goes to stdout verbatim — suppress the human-mode cache
        // notice so the output stays pipeable.
        let bridged = bridge_project(&project_path, no_cache, true)?;
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
        let depth = parse_int_js(depth_arg).unwrap_or(2).clamp(1, 64) as usize;
        let Some(report) = analysis_reports::export_report(&bridged.graph, seed.as_ref(), depth)
        else {
            info(&format!(
                "Symbol \"{}\" not found in the analysis graph",
                symbol.unwrap_or_default()
            ));
            return Ok(());
        };

        if json {
            return print_report_json("export", &report);
        }

        print!("{}", report.dot);
        let _ = io::stdout().flush();
        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze export failed: {msg}"));
        process::exit(1);
    }
}

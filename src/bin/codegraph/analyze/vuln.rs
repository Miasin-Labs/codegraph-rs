use super::*;

pub(crate) fn cmd_analyze_vuln(
    min_confidence_arg: &str,
    path_arg: Option<&str>,
    no_cache: bool,
    sarif_path: Option<&str>,
    html_path: Option<&str>,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let min_confidence = min_confidence_arg
            .parse::<f64>()
            .unwrap_or(0.5)
            .clamp(0.0, 1.0);
        let report = analysis_reports::vuln_report(&bridged.graph, &project_path, min_confidence);

        // Side-channel exports run regardless of stdout format.
        if let Some(p) = sarif_path {
            let sarif = serde_json::to_string_pretty(&report.to_sarif())
                .map_err(|e| format!("serialize SARIF: {e}"))?;
            std::fs::write(p, sarif).map_err(|e| format!("write SARIF to {p}: {e}"))?;
            if !json {
                info(&format!(
                    "Wrote SARIF log ({} finding(s)) to {p}",
                    report.findings.len()
                ));
            }
        }
        if let Some(p) = html_path {
            std::fs::write(p, report.to_html()).map_err(|e| format!("write HTML to {p}: {e}"))?;
            if !json {
                info(&format!("Wrote HTML report to {p}"));
            }
        }

        if json {
            return print_report_json("vuln", &report);
        }
        print!("{}", report.render_human());
        if report.findings.is_empty() {
            info(
                "No findings at or above the confidence threshold (lower --min-confidence to widen)",
            );
        }
        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze vuln failed: {msg}"));
        process::exit(1);
    }
}

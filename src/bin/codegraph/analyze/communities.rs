use super::{
    analysis_reports,
    bold,
    bridge_project,
    cyan,
    dim,
    error_msg,
    format_number,
    info,
    js_to_fixed,
    parse_int_js,
    print_report_json,
    process,
    resolve_project_path,
};

/// codegraph analyze communities
pub(crate) fn cmd_analyze_communities(
    path_arg: Option<&str>,
    sample_arg: &str,
    no_cache: bool,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let sample = parse_int_js(sample_arg).unwrap_or(8).max(1) as usize;
        let report = analysis_reports::communities_report(&bridged.graph, sample);

        if json {
            return print_report_json("communities", &report);
        }

        if report.communities.is_empty() {
            info("No multi-member call-graph communities found");
            return Ok(());
        }

        println!(
            "{}",
            bold(&format!(
                "\nCall-graph communities ({} multi-member, modularity {}):\n",
                report.multi_member_count,
                js_to_fixed(report.modularity, 2)
            ))
        );
        for community in &report.communities {
            println!(
                "{} {}",
                cyan(&format!("Community {}", community.id)),
                dim(&format!("({} symbols)", community.size))
            );
            if !community.top_files.is_empty() {
                println!(
                    "{}",
                    dim(&format!("  files: {}", community.top_files.join(", ")))
                );
            }
            let names: Vec<&str> = community.members.iter().map(|m| m.name.as_str()).collect();
            let more = if community.truncated {
                format!(" (+{} more)", community.size - community.members.len())
            } else {
                String::new()
            };
            println!("  {}{}", names.join(", "), dim(&more));
            println!();
        }
        info(&format!(
            "{} symbols without call relationships remain singleton communities",
            format_number(report.singleton_count as u64)
        ));

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze communities failed: {msg}"));
        process::exit(1);
    }
}

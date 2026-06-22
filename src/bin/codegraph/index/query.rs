use super::*;

/// codegraph query <search>
pub(crate) fn cmd_query(
    search: &str,
    path_arg: Option<&str>,
    limit_arg: &str,
    kind: Option<&str>,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        if !is_initialized(&project_path) {
            error_msg(&format!(
                "CodeGraph not initialized in {}",
                project_path.display()
            ));
            process::exit(1);
        }

        let cg =
            CodeGraph::open(&project_path, &OpenOptions::default()).map_err(|e| e.to_string())?;

        let limit = parse_int_js(limit_arg).unwrap_or(10).max(0) as usize;
        // TS passes an unvalidated kind string straight to the SQL filter; an
        // unknown kind matches no rows. NodeKind is an enum here, so an
        // unparseable kind short-circuits to the same empty result set.
        let (kinds, kind_invalid) = match kind {
            Some(k) => match k.parse::<NodeKind>() {
                Ok(nk) => (Some(vec![nk]), false),
                Err(_) => (None, true),
            },
            None => (None, false),
        };
        let raw_results = if kind_invalid {
            Vec::new()
        } else {
            cg.search_nodes(
                search,
                Some(&SearchOptions {
                    limit: Some(limit),
                    kinds,
                    ..Default::default()
                }),
            )
            .map_err(|e| e.to_string())?
        };

        // Mirror the MCP search down-rank so the CLI also surfaces the
        // hand-written implementation before protobuf/gRPC scaffolding
        // when both share a name. See extraction/generated-detection.
        let mut results = raw_results;
        results.sort_by_key(|r| {
            if is_generated_file(&r.node.file_path) {
                1
            } else {
                0
            }
        });

        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&results).map_err(|e| e.to_string())?
            );
        } else if results.is_empty() {
            info(&format!("No results found for \"{search}\""));
        } else {
            println!("{}", bold(&format!("\nSearch Results for \"{search}\":\n")));

            // Human display only: relevance relative to the best hit (top = 100%).
            // Raw scores stack FTS bm25 + name/kind/path bonuses and routinely
            // exceed 1.0, so an absolute percent reads as nonsense like "(11012%)".
            // JSON output keeps the raw parity-faithful score.
            let max_score = results.iter().map(|r| r.score).fold(f64::EPSILON, f64::max);

            for result in &results {
                let node = &result.node;
                let location = format!("{}:{}", node.file_path, node.start_line);
                let score = dim(&format!(
                    "({}%)",
                    js_to_fixed((result.score / max_score) * 100.0, 0)
                ));

                println!(
                    "{}{} {score}",
                    cyan(&format!("{:<12}", node.kind.as_str())),
                    white(&node.name)
                );
                println!("{}", dim(&format!("  {location}")));
                if let Some(signature) = &node.signature {
                    println!("{}", dim(&format!("  {signature}")));
                }
                println!();
            }
        }

        cg.close();
        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("Search failed: {msg}"));
        process::exit(1);
    }
}

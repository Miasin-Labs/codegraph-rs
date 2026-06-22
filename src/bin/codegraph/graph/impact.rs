use super::*;

/// codegraph impact <symbol>
pub(crate) fn cmd_impact(symbol: &str, path_arg: Option<&str>, depth_arg: &str, json: bool) {
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
        let depth = parse_int_js(depth_arg).unwrap_or(2).clamp(1, 10) as u32;

        let matches = cg
            .search_nodes(
                symbol,
                Some(&SearchOptions {
                    limit: Some(50),
                    ..Default::default()
                }),
            )
            .map_err(|e| e.to_string())?;
        if matches.is_empty() {
            info(&format!("Symbol \"{symbol}\" not found"));
            cg.close();
            return Ok(());
        }

        // Merge impact subgraphs across all exact-matching symbols
        let mut merged_nodes: HashMap<String, (String, String, String, u32)> = HashMap::new();
        let mut seen_edges: HashSet<String> = HashSet::new();
        let mut edge_count = 0usize;

        for m in &matches {
            let exact_match = is_exact_symbol_match(&m.node.name, symbol);
            if !exact_match && matches.len() > 1 {
                continue;
            }
            let impact = cg
                .get_impact_radius(&m.node.id, Some(depth))
                .map_err(|e| e.to_string())?;
            for (id, n) in &impact.nodes {
                merged_nodes.insert(
                    id.clone(),
                    (
                        n.name.clone(),
                        n.kind.as_str().to_string(),
                        n.file_path.clone(),
                        n.start_line,
                    ),
                );
            }
            for e in &impact.edges {
                let key = format!("{}->{}:{}", e.source, e.target, e.kind.as_str());
                if seen_edges.insert(key) {
                    edge_count += 1;
                }
            }
        }

        // Fallback to top match if exact filter removed everything
        if merged_nodes.is_empty() {
            if let Some(first) = matches.first() {
                let impact = cg
                    .get_impact_radius(&first.node.id, Some(depth))
                    .map_err(|e| e.to_string())?;
                for (id, n) in &impact.nodes {
                    merged_nodes.insert(
                        id.clone(),
                        (
                            n.name.clone(),
                            n.kind.as_str().to_string(),
                            n.file_path.clone(),
                            n.start_line,
                        ),
                    );
                }
                edge_count = impact.edges.len();
            }
        }

        // The TS Map preserved BFS insertion order; the subgraph's HashMap
        // loses it, so emit a deterministic (filePath, startLine, name) order.
        let mut affected: Vec<(String, String, String, u32)> = merged_nodes.into_values().collect();
        affected.sort_by(|a, b| a.2.cmp(&b.2).then(a.3.cmp(&b.3)).then(a.0.cmp(&b.0)));

        if json {
            let entries: Vec<serde_json::Value> = affected
                .iter()
                .map(|(name, kind, file_path, start_line)| {
                    serde_json::json!({
                        "name": name,
                        "kind": kind,
                        "filePath": file_path,
                        "startLine": start_line,
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "symbol": symbol,
                    "depth": depth,
                    "nodeCount": affected.len(),
                    "edgeCount": edge_count,
                    "affected": entries,
                }))
                .map_err(|e| e.to_string())?
            );
        } else if affected.is_empty() {
            info(&format!("No affected symbols found for \"{symbol}\""));
        } else {
            println!(
                "{}",
                bold(&format!(
                    "\nImpact of changing \"{symbol}\" — {} affected symbols:\n",
                    affected.len()
                ))
            );

            // Group by file (insertion order over the sorted affected list)
            let mut by_file: Vec<(String, Vec<(String, String, u32)>)> = Vec::new();
            for (name, kind, file_path, start_line) in &affected {
                match by_file.iter_mut().find(|(f, _)| f == file_path) {
                    Some((_, list)) => list.push((name.clone(), kind.clone(), *start_line)),
                    None => by_file.push((
                        file_path.clone(),
                        vec![(name.clone(), kind.clone(), *start_line)],
                    )),
                }
            }

            for (file, nodes) in &by_file {
                println!("{}", cyan(file));
                for (name, kind, start_line) in nodes {
                    let loc = if *start_line != 0 {
                        format!(":{start_line}")
                    } else {
                        String::new()
                    };
                    println!("  {}{name}{}", dim(&format!("{kind:<12}")), dim(&loc));
                }
                println!();
            }
        }

        cg.close();
        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("impact failed: {msg}"));
        process::exit(1);
    }
}

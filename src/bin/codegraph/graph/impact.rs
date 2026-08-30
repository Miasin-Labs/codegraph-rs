use super::{
    CodeGraph,
    HashMap,
    HashSet,
    OpenOptions,
    SearchOptions,
    bold,
    cyan,
    dim,
    error_msg,
    group_definitions,
    info,
    is_exact_symbol_match,
    is_initialized,
    parse_int_js,
    process,
    resolve_project_path,
};

// (name, kind, filePath, startLine)
type ImpactNode = (String, String, String, u32);

/// Compute one definition's blast radius: the union of impact subgraphs over
/// every node that maps to that definition, deduped by node id and edge key.
fn impact_of(
    cg: &CodeGraph,
    node_ids: &[String],
    depth: u32,
) -> Result<(Vec<ImpactNode>, usize), String> {
    let mut merged_nodes: HashMap<String, ImpactNode> = HashMap::new();
    let mut seen_edges: HashSet<String> = HashSet::new();
    let mut edge_count = 0usize;
    for id in node_ids {
        let impact = cg
            .get_impact_radius(id, Some(depth))
            .map_err(|e| e.to_string())?;
        for (nid, n) in &impact.nodes {
            merged_nodes.insert(
                nid.clone(),
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
    // The TS Map preserved BFS insertion order; the subgraph's HashMap loses
    // it, so emit a deterministic (filePath, startLine, name) order.
    let mut affected: Vec<ImpactNode> = merged_nodes.into_values().collect();
    affected.sort_by(|a, b| a.2.cmp(&b.2).then(a.3.cmp(&b.3)).then(a.0.cmp(&b.0)));
    Ok((affected, edge_count))
}

/// Print one grouped-by-file impact block for the human output.
fn print_impact_by_file(affected: &[ImpactNode], indent: &str) {
    let mut by_file: Vec<(String, Vec<(String, String, u32)>)> = Vec::new();
    for (name, kind, file_path, start_line) in affected {
        match by_file.iter_mut().find(|(f, _)| f == file_path) {
            Some((_, list)) => list.push((name.clone(), kind.clone(), *start_line)),
            None => by_file.push((
                file_path.clone(),
                vec![(name.clone(), kind.clone(), *start_line)],
            )),
        }
    }
    for (file, nodes) in &by_file {
        println!("{indent}{}", cyan(file));
        for (name, kind, start_line) in nodes {
            let loc = if *start_line != 0 {
                format!(":{start_line}")
            } else {
                String::new()
            };
            println!(
                "{indent}  {}{name}{}",
                dim(&format!("{kind:<12}")),
                dim(&loc)
            );
        }
        println!();
    }
}

/// codegraph impact <symbol>
pub(crate) fn cmd_impact(
    symbol: &str,
    path_arg: Option<&str>,
    file_arg: Option<&str>,
    depth_arg: &str,
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

        // Which matched nodes answer for the typed symbol: exact-name matches
        // when the name exists several times, else the single top hit.
        let mut chosen: Vec<codegraph::Node> = matches
            .iter()
            .filter(|m| is_exact_symbol_match(&m.node.name, symbol) || matches.len() == 1)
            .map(|m| m.node.clone())
            .collect();
        if chosen.is_empty() {
            if let Some(first) = matches.first() {
                chosen.push(first.node.clone());
            }
        }

        let (groups, filtered_out) = group_definitions(&chosen, file_arg);
        let filter_note = if filtered_out {
            file_arg.map(|f| {
                format!(
                    "no definition of \"{symbol}\" matches file \"{f}\" — showing all definitions instead"
                )
            })
        } else {
            None
        };

        // One blast radius per distinct definition — merging unrelated
        // same-named defs overstated impact under a single symbol.
        let mut per_def: Vec<(&super::calls::DefinitionGroup, Vec<ImpactNode>, usize)> = Vec::new();
        for group in &groups {
            let (affected, edge_count) = impact_of(&cg, &group.node_ids, depth)?;
            per_def.push((group, affected, edge_count));
        }

        let single = groups.len() == 1;

        if json {
            let affected_json = |affected: &[ImpactNode]| -> Vec<serde_json::Value> {
                affected
                    .iter()
                    .map(|(name, kind, file_path, start_line)| {
                        serde_json::json!({
                            "name": name,
                            "kind": kind,
                            "filePath": file_path,
                            "startLine": start_line,
                        })
                    })
                    .collect()
            };

            let mut obj = serde_json::Map::new();
            obj.insert("symbol".to_string(), serde_json::json!(symbol));
            obj.insert("depth".to_string(), serde_json::json!(depth));
            if let Some(note) = &filter_note {
                obj.insert("note".to_string(), serde_json::json!(note));
            }
            if single {
                let (_, affected, edge_count) = per_def
                    .first()
                    .map(|(g, a, e)| (*g, a.clone(), *e))
                    .unwrap_or_else(|| {
                        // Unreachable in practice: chosen is non-empty here.
                        (groups.first().expect("one group"), Vec::new(), 0)
                    });
                obj.insert("nodeCount".to_string(), serde_json::json!(affected.len()));
                obj.insert("edgeCount".to_string(), serde_json::json!(edge_count));
                obj.insert(
                    "affected".to_string(),
                    serde_json::Value::Array(affected_json(&affected)),
                );
            } else {
                let defs: Vec<serde_json::Value> = per_def
                    .iter()
                    .map(|(group, affected, edge_count)| {
                        serde_json::json!({
                            "definition": {
                                "qualifiedName": group.qualified_name,
                                "kind": group.kind,
                                "filePath": group.file_path,
                                "startLine": group.start_line,
                            },
                            "nodeCount": affected.len(),
                            "edgeCount": edge_count,
                            "affected": affected_json(affected),
                        })
                    })
                    .collect();
                obj.insert("definitions".to_string(), serde_json::Value::Array(defs));
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::Value::Object(obj))
                    .map_err(|e| e.to_string())?
            );
        } else if single {
            let (_, affected, _) = &per_def[0];
            if affected.is_empty() {
                info(&format!("No affected symbols found for \"{symbol}\""));
            } else {
                println!(
                    "{}",
                    bold(&format!(
                        "\nImpact of changing \"{symbol}\" — {} affected symbols:\n",
                        affected.len()
                    ))
                );
                print_impact_by_file(affected, "");
            }
            if let Some(note) = &filter_note {
                println!("{}", dim(&format!("Note: {note}")));
            }
        } else {
            println!(
                "{}",
                bold(&format!(
                    "\nImpact of \"{symbol}\" — {} distinct definitions (each with its own blast radius; narrow with --file):\n",
                    groups.len()
                ))
            );
            for (group, affected, _) in &per_def {
                let head_loc = if group.start_line != 0 {
                    format!(":{}", group.start_line)
                } else {
                    String::new()
                };
                println!(
                    "{}",
                    bold(&format!(
                        "{} ({}) — {}{} — {} affected:",
                        group.qualified_name,
                        group.kind,
                        group.file_path,
                        head_loc,
                        affected.len()
                    ))
                );
                if affected.is_empty() {
                    println!("  (none)");
                    println!();
                } else {
                    print_impact_by_file(affected, "  ");
                }
            }
            if let Some(note) = &filter_note {
                println!("{}", dim(&format!("Note: {note}")));
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

use codegraph::federation::{CrossImpact, ForeignSymbol, GraphSet};

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

/// Symbols of a blast radius whose dependents are looked up across projects.
const CROSS_CHANGED: usize = 256;
/// Symbols listed per dependent project.
const PER_PROJECT: usize = 40;

/// Compute one definition's blast radius: the union of impact subgraphs over
/// every node that maps to that definition, deduped by node id and edge key.
fn impact_of(
    cg: &CodeGraph,
    node_ids: &[String],
    depth: u32,
) -> Result<(Vec<ImpactNode>, usize, Vec<codegraph::Node>), String> {
    let mut merged_nodes: HashMap<String, ImpactNode> = HashMap::new();
    let mut changed: HashMap<String, codegraph::Node> = HashMap::new();
    let mut seen_edges: HashSet<String> = HashSet::new();
    let mut edge_count = 0usize;
    for id in node_ids {
        let impact = cg
            .get_impact_radius(id, Some(depth))
            .map_err(|e| e.to_string())?;
        for (nid, n) in &impact.nodes {
            if changed.len() < CROSS_CHANGED {
                changed.insert(nid.clone(), n.clone());
            }
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
    Ok((affected, edge_count, changed.into_values().collect()))
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
        // A path naming a dependency or a linked project is measured in
        // that graph, then in every project using it.
        let fed = super::federated::reader();
        if !matches
            .iter()
            .any(|m| is_exact_symbol_match(&m.node.name, symbol))
        {
            if let Some(fed) = &fed {
                let foreign = super::federated::foreign_symbols(fed, &project_path, symbol);
                if !foreign.is_empty() {
                    print_foreign_impact(fed, &project_path, &foreign, depth, json)?;
                    cg.close();
                    return Ok(());
                }
            }
        }
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
        let mut others: Vec<Option<CrossImpact>> = Vec::new();
        for group in &groups {
            let (affected, edge_count, changed) = impact_of(&cg, &group.node_ids, depth)?;
            per_def.push((group, affected, edge_count));
            // The same blast radius in the projects that use this code.
            others.push(fed.as_ref().and_then(|fed| {
                let cross = fed.impact_across(
                    &super::federated::project_graph(&project_path),
                    &changed,
                    None,
                    depth,
                    PER_PROJECT,
                );
                (!cross.is_empty()).then_some(cross)
            }));
        }
        let add_others = |obj: &mut serde_json::Value, cross: &Option<CrossImpact>| {
            if let Some(cross) = cross {
                let (groups, skipped) = super::federated::cross_impact_json(cross);
                obj["otherProjects"] = groups;
                obj["skippedProjects"] = skipped;
            }
        };
        let print_others = |cross: &Option<CrossImpact>| {
            if let Some(cross) = cross {
                super::federated::print_section(&super::federated::impact_text(cross));
            }
        };

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
                if let Some(Some(cross)) = others.first() {
                    let (groups, skipped) = super::federated::cross_impact_json(cross);
                    obj.insert("otherProjects".to_string(), groups);
                    obj.insert("skippedProjects".to_string(), skipped);
                }
            } else {
                let defs: Vec<serde_json::Value> = per_def
                    .iter()
                    .zip(&others)
                    .map(|((group, affected, edge_count), cross)| {
                        let mut def = serde_json::json!({
                            "definition": {
                                "qualifiedName": group.qualified_name,
                                "kind": group.kind,
                                "filePath": group.file_path,
                                "startLine": group.start_line,
                            },
                            "nodeCount": affected.len(),
                            "edgeCount": edge_count,
                            "affected": affected_json(affected),
                        });
                        add_others(&mut def, cross);
                        def
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
            let cross = others.first().cloned().flatten();
            if affected.is_empty() && cross.is_none() {
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
                print_others(&cross);
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
            for ((group, affected, _), cross) in per_def.iter().zip(&others) {
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
                print_others(cross);
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

/// The blast radius of symbols that live in another graph: inside it, then
/// in every project that uses it.
fn print_foreign_impact(
    fed: &GraphSet,
    project_path: &std::path::Path,
    foreign: &[ForeignSymbol],
    depth: u32,
    json: bool,
) -> Result<(), String> {
    let mut defs = Vec::new();
    for found in foreign {
        let radius = found
            .graph
            .traverser()
            .get_impact_radius(&found.node.id, depth)
            .map_err(|e| e.to_string())?;
        let mut inside: Vec<codegraph::Node> = radius.nodes.into_values().collect();
        inside.sort_by(|a, b| {
            a.file_path
                .cmp(&b.file_path)
                .then(a.start_line.cmp(&b.start_line))
        });
        let changed: Vec<codegraph::Node> = inside.iter().take(CROSS_CHANGED).cloned().collect();
        let cross = fed.impact_across(
            &found.graph.id,
            &changed,
            Some(project_path),
            depth,
            PER_PROJECT,
        );
        if json {
            let (groups, skipped) = super::federated::cross_impact_json(&cross);
            defs.push(serde_json::json!({
                "graph": found.graph.label,
                "definition": {
                    "qualifiedName": found.node.qualified_name,
                    "kind": found.node.kind.as_str(),
                    "filePath": found.graph.path_of(&found.node.file_path),
                    "startLine": found.node.start_line,
                },
                "affected": inside.iter().map(super::federated::node_json).collect::<Vec<_>>(),
                "otherProjects": groups,
                "skippedProjects": skipped,
            }));
            continue;
        }
        println!(
            "{}",
            bold(&format!(
                "\nImpact of changing {} ({}) in {} — {} affected there:\n",
                found.node.qualified_name,
                found.node.kind.as_str(),
                found.graph.label,
                inside.len()
            ))
        );
        let affected: Vec<ImpactNode> = inside
            .iter()
            .map(|n| {
                (
                    n.name.clone(),
                    n.kind.as_str().to_string(),
                    n.file_path.clone(),
                    n.start_line,
                )
            })
            .collect();
        print_impact_by_file(&affected, "");
        super::federated::print_section(&super::federated::impact_text(&cross));
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "definitions": defs }))
                .map_err(|e| e.to_string())?
        );
    }
    Ok(())
}

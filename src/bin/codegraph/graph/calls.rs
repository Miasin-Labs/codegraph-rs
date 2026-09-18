use super::{
    CodeGraph,
    HashSet,
    OpenOptions,
    SearchOptions,
    bold,
    cyan,
    dim,
    error_msg,
    info,
    is_initialized,
    parse_int_js,
    process,
    resolve_project_path,
    white,
};

// =============================================================================
// callers / callees
//
// CLI parity with the MCP graph tools (codegraph_callers/callees/impact) so
// the traversal queries work in scripts, CI, and git hooks without a running
// MCP server.
// =============================================================================

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum CallDirection {
    Callers,
    Callees,
}

impl CallDirection {
    fn noun(self) -> &'static str {
        match self {
            CallDirection::Callers => "callers",
            CallDirection::Callees => "callees",
        }
    }
    fn heading(self) -> &'static str {
        match self {
            CallDirection::Callers => "Callers",
            CallDirection::Callees => "Callees",
        }
    }
}

/// Is `name` an exact match for `symbol` (allowing `.`/`::` qualification)?
pub(crate) fn is_exact_symbol_match(name: &str, symbol: &str) -> bool {
    name == symbol
        || name.ends_with(&format!(".{symbol}"))
        || name.ends_with(&format!("::{symbol}"))
}

/// One distinct definition: its identifying node fields plus the ids of every
/// node that maps to it (same `(filePath, qualifiedName)`), so same-file
/// overloads stay together while same-named defs in different files stay apart.
pub(crate) struct DefinitionGroup {
    pub qualified_name: String,
    pub kind: String,
    pub file_path: String,
    pub start_line: u32,
    pub node_ids: Vec<String>,
}

/// Group matched nodes into DISTINCT DEFINITIONS — one group per
/// `(filePath, qualifiedName)` — mirroring the MCP `groupDefinitions` helper so
/// the two surfaces answer the same question the same way. Optionally narrow to
/// a `file` path/suffix first; if the filter matches nothing, keep all groups
/// and report it via the returned `filtered_out` flag.
pub(crate) fn group_definitions(
    nodes: &[codegraph::Node],
    file_filter: Option<&str>,
) -> (Vec<DefinitionGroup>, bool) {
    let mut pool: Vec<&codegraph::Node> = nodes.iter().collect();
    let mut filtered_out = false;
    if let Some(filter) = file_filter {
        let wanted = filter.strip_prefix("./").unwrap_or(filter);
        let narrowed: Vec<&codegraph::Node> = pool
            .iter()
            .copied()
            .filter(|n| {
                n.file_path == wanted
                    || n.file_path.ends_with(wanted)
                    || n.file_path.ends_with(&format!("/{wanted}"))
            })
            .collect();
        if narrowed.is_empty() {
            filtered_out = true;
        } else {
            pool = narrowed;
        }
    }

    // Preserve first-seen order (TS keyed a `Map`, which is insertion-ordered).
    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, DefinitionGroup> =
        std::collections::HashMap::new();
    for n in pool {
        let key = format!("{}|{}", n.file_path, n.qualified_name);
        match groups.get_mut(&key) {
            Some(group) => group.node_ids.push(n.id.clone()),
            None => {
                order.push(key.clone());
                groups.insert(
                    key,
                    DefinitionGroup {
                        qualified_name: n.qualified_name.clone(),
                        kind: n.kind.as_str().to_string(),
                        file_path: n.file_path.clone(),
                        start_line: n.start_line,
                        node_ids: vec![n.id.clone()],
                    },
                );
            }
        }
    }
    let ordered = order
        .into_iter()
        .filter_map(|k| groups.remove(&k))
        .collect();
    (ordered, filtered_out)
}

/// codegraph callers <symbol> / codegraph callees <symbol>
pub(crate) fn cmd_call_graph(
    direction: CallDirection,
    symbol: &str,
    path_arg: Option<&str>,
    file_arg: Option<&str>,
    limit_arg: &str,
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
        let limit = parse_int_js(limit_arg).unwrap_or(20).max(0) as usize;

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

        let fetch = |node_id: &str| -> Result<Vec<codegraph::NodeRef>, String> {
            match direction {
                CallDirection::Callers => cg.get_callers(node_id, None),
                CallDirection::Callees => cg.get_callees(node_id, None),
            }
            .map_err(|e| e.to_string())
        };

        // Collect one deduped edge set per distinct definition.
        type Related = Vec<(String, String, String, u32)>; // (name, kind, filePath, startLine)
        let mut per_def: Vec<(&DefinitionGroup, Related)> = Vec::new();
        for group in &groups {
            let mut seen: HashSet<String> = HashSet::new();
            let mut related: Related = Vec::new();
            for id in &group.node_ids {
                for c in fetch(id)? {
                    if seen.insert(c.node.id.clone()) {
                        related.push((
                            c.node.name.clone(),
                            c.node.kind.as_str().to_string(),
                            c.node.file_path.clone(),
                            c.node.start_line,
                        ));
                    }
                }
            }
            related.truncate(limit);
            per_def.push((group, related));
        }
        // Callees in other graphs (dependency shards, linked projects),
        // per definition — listed after the project's own.
        let externals: Vec<Vec<codegraph::db::ExternalEdge>> = groups
            .iter()
            .map(|group| {
                if direction != CallDirection::Callees {
                    return Ok(Vec::new());
                }
                let mut seen: HashSet<(String, String)> = HashSet::new();
                let mut edges = cg
                    .get_external_edges(&group.node_ids)
                    .map_err(|e| e.to_string())?;
                edges.retain(|edge| {
                    matches!(
                        edge.kind,
                        codegraph::EdgeKind::Calls | codegraph::EdgeKind::Instantiates
                    ) && seen.insert((edge.target_graph_key.clone(), edge.target_node_id.clone()))
                });
                edges.truncate(limit);
                Ok(edges)
            })
            .collect::<Result<_, String>>()?;
        let graph_label = |key: &str| key.rsplit('/').next().unwrap_or(key).to_string();
        let external_json = |edges: &[codegraph::db::ExternalEdge]| -> Vec<serde_json::Value> {
            edges
                .iter()
                .map(|edge| {
                    serde_json::json!({
                        "graph": edge.target_graph_key,
                        "graphKind": edge.target_graph_kind.as_str(),
                        "name": edge.target_name,
                        "qualifiedName": edge.target_qualified_name,
                        "kind": edge.target_kind.as_str(),
                        "filePath": edge.target_file_path,
                        "startLine": edge.target_line,
                    })
                })
                .collect()
        };
        let print_external = |edges: &[codegraph::db::ExternalEdge], indent: &str| {
            if edges.is_empty() {
                return;
            }
            println!(
                "{}",
                bold(&format!("{indent}In other graphs ({}):", edges.len()))
            );
            for edge in edges {
                println!(
                    "{indent}  {}{} {}",
                    cyan(&format!("{:<12}", edge.target_kind.as_str())),
                    white(&edge.target_qualified_name),
                    dim(&format!(
                        "{} {}:{}",
                        graph_label(&edge.target_graph_key),
                        edge.target_file_path,
                        edge.target_line.unwrap_or_default()
                    ))
                );
            }
            println!();
        };

        let single = groups.len() == 1;

        if json {
            let related_json = |related: &Related| -> Vec<serde_json::Value> {
                related
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
            if let Some(note) = &filter_note {
                obj.insert("note".to_string(), serde_json::json!(note));
            }
            if single {
                // Familiar flat envelope: `{ symbol, callers|callees }`.
                let related = per_def.first().map(|(_, r)| r.clone()).unwrap_or_default();
                obj.insert(
                    direction.noun().to_string(),
                    serde_json::Value::Array(related_json(&related)),
                );
                if let Some(external) = externals.first().filter(|edges| !edges.is_empty()) {
                    obj.insert(
                        "external".to_string(),
                        serde_json::Value::Array(external_json(external)),
                    );
                }
            } else {
                // Multiple distinct definitions: nest edges under their def so
                // attribution survives (a flat union cannot express it).
                let defs: Vec<serde_json::Value> = per_def
                    .iter()
                    .zip(&externals)
                    .map(|((group, related), external)| {
                        let mut def = serde_json::json!({
                            "definition": {
                                "qualifiedName": group.qualified_name,
                                "kind": group.kind,
                                "filePath": group.file_path,
                                "startLine": group.start_line,
                            },
                            direction.noun(): related_json(related),
                        });
                        if !external.is_empty() {
                            def["external"] = serde_json::Value::Array(external_json(external));
                        }
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
            let related = per_def.first().map(|(_, r)| r.clone()).unwrap_or_default();
            let external = externals.first().cloned().unwrap_or_default();
            if related.is_empty() && external.is_empty() {
                info(&format!("No {} found for \"{symbol}\"", direction.noun()));
            } else if related.is_empty() {
                println!();
                print_external(&external, "");
            } else {
                println!(
                    "{}",
                    bold(&format!(
                        "\n{} of \"{symbol}\" ({}):\n",
                        direction.heading(),
                        related.len()
                    ))
                );
                for (name, kind, file_path, start_line) in &related {
                    let loc = if *start_line != 0 {
                        format!(":{start_line}")
                    } else {
                        String::new()
                    };
                    println!("{}{}", cyan(&format!("{kind:<12}")), white(name));
                    println!("{}", dim(&format!("  {file_path}{loc}")));
                    println!();
                }
                print_external(&external, "");
            }
            if let Some(note) = &filter_note {
                println!("{}", dim(&format!("Note: {note}")));
            }
        } else {
            // Multiple distinct definitions: one attributed section each so a
            // consumer never mistakes one definition's edges for another's.
            println!(
                "{}",
                bold(&format!(
                    "\n{} of \"{symbol}\" — {} distinct definitions (narrow with --file):\n",
                    direction.heading(),
                    groups.len()
                ))
            );
            for ((group, related), external) in per_def.iter().zip(&externals) {
                let head_loc = if group.start_line != 0 {
                    format!(":{}", group.start_line)
                } else {
                    String::new()
                };
                println!(
                    "{}",
                    bold(&format!(
                        "{} ({}) — {}{}",
                        group.qualified_name, group.kind, group.file_path, head_loc
                    ))
                );
                if related.is_empty() {
                    println!("{}", dim(&format!("  (no {})", direction.noun())));
                } else {
                    for (name, kind, file_path, start_line) in related {
                        let loc = if *start_line != 0 {
                            format!(":{start_line}")
                        } else {
                            String::new()
                        };
                        println!(
                            "  {}{} {}",
                            cyan(&format!("{kind:<12}")),
                            white(name),
                            dim(&format!("{file_path}{loc}"))
                        );
                    }
                }
                println!();
                print_external(external, "  ");
            }
            if let Some(note) = &filter_note {
                println!("{}", dim(&format!("Note: {note}")));
            }
        }

        cg.close();
        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("{} failed: {msg}", direction.noun()));
        process::exit(1);
    }
}

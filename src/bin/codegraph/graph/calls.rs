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

/// codegraph callers <symbol> / codegraph callees <symbol>
pub(crate) fn cmd_call_graph(
    direction: CallDirection,
    symbol: &str,
    path_arg: Option<&str>,
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

        let fetch = |node_id: &str| -> Result<Vec<codegraph::NodeRef>, String> {
            match direction {
                CallDirection::Callers => cg.get_callers(node_id, None),
                CallDirection::Callees => cg.get_callees(node_id, None),
            }
            .map_err(|e| e.to_string())
        };

        let mut seen: HashSet<String> = HashSet::new();
        let mut all: Vec<(String, String, String, u32)> = Vec::new(); // (name, kind, filePath, startLine)

        for m in &matches {
            let exact_match = is_exact_symbol_match(&m.node.name, symbol);
            if !exact_match && matches.len() > 1 {
                continue;
            }
            for c in fetch(&m.node.id)? {
                if seen.insert(c.node.id.clone()) {
                    all.push((
                        c.node.name.clone(),
                        c.node.kind.as_str().to_string(),
                        c.node.file_path.clone(),
                        c.node.start_line,
                    ));
                }
            }
        }

        // Fallback: if exact filter removed everything, use the top match
        if all.is_empty() {
            if let Some(first) = matches.first() {
                for c in fetch(&first.node.id)? {
                    if seen.insert(c.node.id.clone()) {
                        all.push((
                            c.node.name.clone(),
                            c.node.kind.as_str().to_string(),
                            c.node.file_path.clone(),
                            c.node.start_line,
                        ));
                    }
                }
            }
        }

        let limited = &all[..all.len().min(limit)];

        if json {
            let entries: Vec<serde_json::Value> = limited
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
            // `{ symbol, callers }` / `{ symbol, callees }` — the key name
            // follows the command, so build the object manually.
            let mut obj = serde_json::Map::new();
            obj.insert("symbol".to_string(), serde_json::json!(symbol));
            obj.insert(
                direction.noun().to_string(),
                serde_json::Value::Array(entries),
            );
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::Value::Object(obj))
                    .map_err(|e| e.to_string())?
            );
        } else if limited.is_empty() {
            info(&format!("No {} found for \"{symbol}\"", direction.noun()));
        } else {
            println!(
                "{}",
                bold(&format!(
                    "\n{} of \"{symbol}\" ({}):\n",
                    direction.heading(),
                    limited.len()
                ))
            );
            for (name, kind, file_path, start_line) in limited {
                let loc = if *start_line != 0 {
                    format!(":{start_line}")
                } else {
                    String::new()
                };
                println!("{}{}", cyan(&format!("{kind:<12}")), white(name));
                println!("{}", dim(&format!("  {file_path}{loc}")));
                println!();
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

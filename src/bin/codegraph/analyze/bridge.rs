use super::{
    ANodeId,
    BridgeOptions,
    BridgeResult,
    CodeGraph,
    DatabaseConnection,
    HashMap,
    OpenOptions,
    Path,
    QueryBuilder,
    SearchOptions,
    analysis_reports,
    build_analysis_graph_cached_with_options,
    cyan,
    dim,
    error_msg,
    get_database_path,
    info,
    is_exact_symbol_match,
    is_initialized,
    process,
    white,
};

/// Bridge the project's index into the analysis engine, via the snapshot
/// cache unless `no_cache`. Exits (status 1) when the project is not
/// initialized — same contract as the other read commands.
///
/// Bridge options come from the process environment
/// (`CODEGRAPH_ANALYSIS_FIELDS=1` turns on field carrying for every
/// analyze command); [`bridge_project_with_options`] is the explicit-flag
/// variant `context --fields` uses.
pub(crate) fn bridge_project(
    project_path: &Path,
    no_cache: bool,
    json: bool,
) -> Result<BridgeResult, String> {
    bridge_project_with_options(project_path, no_cache, json, &BridgeOptions::from_env())
}

/// [`bridge_project`] with explicit [`BridgeOptions`]. The snapshot cache
/// is keyed by the options, so a graph bridged under one flag state is
/// never served to the other.
pub(crate) fn bridge_project_with_options(
    project_path: &Path,
    no_cache: bool,
    json: bool,
    options: &BridgeOptions,
) -> Result<BridgeResult, String> {
    if !is_initialized(project_path) {
        error_msg(&format!(
            "CodeGraph not initialized in {}",
            project_path.display()
        ));
        info("Run \"codegraph init\" first");
        process::exit(1);
    }
    let conn =
        DatabaseConnection::open(get_database_path(project_path)).map_err(|e| e.to_string())?;
    let queries = QueryBuilder::new(conn.get_db().map_err(|e| e.to_string())?);
    let cached =
        build_analysis_graph_cached_with_options(&queries, project_path, !no_cache, options)
            .map_err(|e| e.to_string())?;
    if cached.from_cache && !json {
        println!("{}", dim("(cached graph)"));
    }
    Ok(cached.result)
}

/// Resolve a user-supplied symbol to its analysis-graph node via the index
/// search, using the same exact-match conventions as `callers`/`callees`/
/// `impact` (exact name or `.`/`::`-qualified suffix wins; otherwise the top
/// search hit that the bridge mapped).
pub(crate) fn resolve_analysis_symbol(
    cg: &CodeGraph,
    id_map: &HashMap<String, ANodeId>,
    symbol: &str,
) -> Result<Option<ANodeId>, String> {
    let matches = cg
        .search_nodes(
            symbol,
            Some(&SearchOptions {
                limit: Some(50),
                ..Default::default()
            }),
        )
        .map_err(|e| e.to_string())?;
    for m in &matches {
        if is_exact_symbol_match(&m.node.name, symbol) || matches.len() == 1 {
            if let Some(aid) = id_map.get(&m.node.id) {
                return Ok(Some(aid.clone()));
            }
        }
    }
    // Fallback: top search hit with an analysis mapping (skipped node kinds
    // like variables/imports have no analysis node).
    for m in &matches {
        if let Some(aid) = id_map.get(&m.node.id) {
            return Ok(Some(aid.clone()));
        }
    }
    Ok(None)
}

/// Resolve a symbol with the host index open/closed around it.
pub(crate) fn resolve_symbol_via_index(
    project_path: &Path,
    id_map: &HashMap<String, ANodeId>,
    symbol: &str,
) -> Result<Option<ANodeId>, String> {
    let cg = CodeGraph::open(project_path, &OpenOptions::default()).map_err(|e| e.to_string())?;
    let resolved = resolve_analysis_symbol(&cg, id_map, symbol);
    cg.close();
    resolved
}

pub(crate) fn print_json<T: serde::Serialize>(value: &T) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|e| e.to_string())?
    );
    Ok(())
}

/// Print an `analyze` report wrapped in the versioned JSON envelope —
/// `{"schemaVersion": N, "kind": "<kind>", "data": …}` (see
/// [`analysis_reports::ReportEnvelope`]). Every `analyze … --json` goes through here.
pub(crate) fn print_report_json<T: serde::Serialize>(
    kind: &'static str,
    data: &T,
) -> Result<(), String> {
    print_json(&analysis_reports::ReportEnvelope::new(kind, data))
}

pub(crate) fn print_symbol_line(kind: &str, name: &str, file: &str, line: u32) {
    let loc = if line != 0 {
        format!(":{line}")
    } else {
        String::new()
    };
    println!("{}{}", cyan(&format!("{kind:<12}")), white(name));
    println!("{}", dim(&format!("  {file}{loc}")));
}

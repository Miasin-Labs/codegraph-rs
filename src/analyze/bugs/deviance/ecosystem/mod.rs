//! Ecosystem deviance: beliefs about library APIs mined from the crates of
//! the cargo cache (`codegraph deps beliefs build`), and the project's
//! calls that depart from a strong one.
//!
//! The in-project templates learn from one project; a library API may be
//! called only once or twice there, too few to believe anything. Here the
//! population is every cached crate that calls it: [`observe`] reads each
//! call's facts from syntax, [`mine`] turns thousands of crates'
//! observations into beliefs (support, z, lift), and [`apply`] reports the
//! project's departures — citing the agreeing crates.
//!
//! A project's library calls come from its index's `external_edges`, plus
//! a read-only resolution of its unresolved references into the
//! toolchain's `std`/`core`/`alloc` (the toolchain shard the project
//! records, else the one the beliefs were mined against).

mod apply;
pub(crate) mod mine;
pub(crate) mod observe;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::time::Duration;

pub(crate) use observe::{ExternalSite, observe};

use super::results::reports_status;
use super::returns::{Returns, returns};
use super::rules;
use crate::analyze::bugs::{Finding, Project};
use crate::codegraph::CodeGraph;
use crate::db::{ExternalEdge, ExternalGraphKind};
use crate::deps::beliefs::LoadedBeliefs;
use crate::deps::beliefs::model::api_key;
use crate::deps::toolchain;
use crate::resolution::external::Reach;
use crate::types::Language;

/// Time the read-only resolution into the toolchain shards may take.
const TOOLCHAIN_BUDGET: Duration = Duration::from_secs(20);

/// Whether a declared Rust return type reports success or failure (the
/// deviance rules' test, shared by mining and applying).
pub(crate) fn is_status(declared: &str) -> bool {
    rules::for_language(Language::Rust).is_some_and(|rules| reports_status(rules, declared))
}

/// The value type a Rust signature declares it returns (`None` for unit
/// or nothing declared).
pub(crate) fn declared_value(signature: Option<&str>) -> Option<String> {
    let rules = rules::for_language(Language::Rust)?;
    match returns(rules, signature) {
        Returns::Value(declared) => Some(declared),
        Returns::Unit | Returns::Unknown => None,
    }
}

/// What applying the beliefs saw, for the report's note.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Applied {
    pub sites: usize,
    /// The toolchain pass ran out of time.
    pub partial: bool,
}

/// The project's departures from `beliefs`.
pub(crate) fn detect(
    cg: &CodeGraph,
    project: &mut Project,
    beliefs: &LoadedBeliefs,
) -> Result<(Vec<Finding>, Applied), String> {
    let mut sites = index_sites(cg)?;
    let mut applied = Applied::default();
    if !beliefs.toolchain.is_empty() {
        let reach = Reach::from_graphs(beliefs.toolchain.clone());
        let (edges, complete) = cg
            .external_edges_read_only(&reach, TOOLCHAIN_BUDGET)
            .map_err(|e| e.to_string())?;
        applied.partial = !complete;
        sites.extend(edge_sites(cg, &edges)?);
    }
    applied.sites = sites.len();
    let observed = observe(project, &sites);
    Ok((
        apply::findings(&beliefs.set, &observed, &is_status),
        applied,
    ))
}

/// The index's own external call edges into dependency shards.
fn index_sites(cg: &CodeGraph) -> Result<Vec<ExternalSite>, String> {
    let conn = cg.query_builder().db().conn();
    let err = |e: rusqlite::Error| e.to_string();
    let mut stmt = conn
        .prepare(
            "SELECT n.file_path, e.line, IFNULL(e.col, 0), e.target_graph_key, \
                    e.target_qualified_name, e.target_name \
             FROM external_edges e JOIN nodes n ON n.id = e.source \
             WHERE e.kind = 'calls' AND e.line IS NOT NULL \
               AND e.target_graph_kind = ?1 AND e.target_kind IN ('function', 'method')",
        )
        .map_err(err)?;
    let rows = stmt
        .query_map([ExternalGraphKind::Dependency.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, u32>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .map_err(err)?;
    let mut sites = Vec::new();
    for row in rows {
        let (file, line, col, key, qualified, name) = row.map_err(err)?;
        // Calls into the toolchain come from the read-only toolchain pass
        // (which sees the same references), never twice.
        if toolchain::graph_key_version(&key).is_some() {
            continue;
        }
        if let Some(api) = graph_api(&key, "", &qualified) {
            sites.push(ExternalSite {
                file,
                line,
                col,
                api,
                name,
            });
        }
    }
    Ok(sites)
}

/// Sites of edges a read-only pass found (their callers' files looked up).
pub(crate) fn edge_sites(
    cg: &CodeGraph,
    edges: &[ExternalEdge],
) -> Result<Vec<ExternalSite>, String> {
    let conn = cg.query_builder().db().conn();
    let err = |e: rusqlite::Error| e.to_string();
    let mut files: HashMap<&str, Option<String>> = HashMap::new();
    let mut stmt = conn
        .prepare("SELECT file_path FROM nodes WHERE id = ?1")
        .map_err(err)?;
    let mut sites = Vec::new();
    for edge in edges {
        if edge.kind.as_str() != "calls"
            || !matches!(edge.target_kind.as_str(), "function" | "method")
        {
            continue;
        }
        let (Some(line), Some(api)) = (
            edge.line,
            graph_api(
                &edge.target_graph_key,
                &edge.target_file_path,
                &edge.target_qualified_name,
            ),
        ) else {
            continue;
        };
        if !files.contains_key(edge.source.as_str()) {
            let file = stmt
                .query_row([&edge.source], |row| row.get::<_, String>(0))
                .ok();
            files.insert(edge.source.as_str(), file);
        }
        let Some(Some(file)) = files.get(edge.source.as_str()) else {
            continue;
        };
        sites.push(ExternalSite {
            file: file.clone(),
            line,
            col: edge.column.unwrap_or(0),
            api,
            name: edge.target_name.clone(),
        });
    }
    Ok(sites)
}

/// `crates/serde_json-1.0.150` + `from_str` → `serde_json@1::from_str`.
/// The toolchain shard holds three crates: its target's `file` names the
/// one (`rust/std-1.101.0-nightly+d080e7dff1b0` + `alloc/src/vec/mod.rs`
/// + `Vec::new` → `alloc@1::Vec::new`).
pub(crate) fn graph_api(key: &str, file: &str, qualified: &str) -> Option<String> {
    if let Some(version) = toolchain::graph_key_version(key) {
        return Some(api_key(toolchain::crate_of_file(file)?, version, qualified));
    }
    let (name, version) = parse_graph_key(key)?;
    Some(api_key(name, version, qualified))
}

/// A dependency graph key's package name and version:
/// `crates/tokio-util-0.7.15` → (`tokio-util`, `0.7.15`).
pub(crate) fn parse_graph_key(key: &str) -> Option<(&str, &str)> {
    let dir = key.strip_prefix("crates/")?;
    let split = dir
        .char_indices()
        .find(|&(i, c)| {
            c == '-'
                && dir[i + 1..]
                    .chars()
                    .next()
                    .is_some_and(|next| next.is_ascii_digit())
        })
        .map(|(i, _)| i)?;
    Some((&dir[..split], &dir[split + 1..]))
}

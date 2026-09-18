//! The graph tools across graphs (federation phase 3): external edges
//! followed into dependency shards and linked projects, symbols that live
//! there, and the projects that use this code.
//!
//! Every answer here is an addition to the project's own: it is bounded by
//! one [`GraphSet`] per call (its deadline, open-graph bound and project
//! caps), and a graph or project that cannot answer is named with the
//! reason instead of failing the call.

use std::collections::HashSet;

use serde_json::{Map, Value};

use super::super::context::ToolHandler;
use crate::codegraph::CodeGraph;
use crate::db::ExternalEdge;
use crate::error::Result;
pub(in crate::mcp::tools) use crate::federation::render::{
    availability_note,
    cross_callers_section,
    cross_impact_section,
    followed_line,
    foreign_node_line,
};
use crate::federation::{FederationOptions, ForeignSymbol, GraphSet, federation_enabled};
use crate::types::EdgeKind;

/// Definitions one foreign name may resolve to.
pub(in crate::mcp::tools) const FOREIGN_MATCHES: usize = 8;

impl ToolHandler {
    /// This call's cross-graph reader, unless `CODEGRAPH_FEDERATION=0`.
    pub(in crate::mcp::tools) fn federation(&self) -> Option<GraphSet> {
        federation_enabled().then(|| self.federation_reader())
    }

    /// A reader of the atlas, the registry and other indexes even when
    /// edge following is off (`CODEGRAPH_FEDERATION=0` stops tools
    /// following edges, not the projects view or a scoped search).
    pub(in crate::mcp::tools) fn federation_reader(&self) -> GraphSet {
        let options = self
            .federation_options
            .borrow()
            .clone()
            .unwrap_or_else(FederationOptions::from_env);
        GraphSet::new(options)
    }

    /// Definitions `symbol` names in the graphs `cg`'s project reaches.
    pub(in crate::mcp::tools) fn foreign_symbols(
        &self,
        fed: &GraphSet,
        cg: &CodeGraph,
        symbol: &str,
        graph: Option<&str>,
    ) -> Vec<ForeignSymbol> {
        fed.resolve_symbol(cg.get_project_root(), symbol, graph, FOREIGN_MATCHES)
    }
}

/// The `graph` argument: the other graph a symbol lives in (a dependency
/// crate name, `name@version`, or a linked project's name).
pub(in crate::mcp::tools) fn graph_arg(args: &Map<String, Value>) -> Option<String> {
    args.get("graph")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|graph| !graph.is_empty())
        .map(str::to_string)
}

/// Edges of `kinds` leaving `ids`, one per target, in source order.
pub(in crate::mcp::tools) fn external_edges_of(
    cg: &CodeGraph,
    ids: &[String],
    kinds: &[EdgeKind],
) -> Result<Vec<ExternalEdge>> {
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut edges = cg.get_external_edges(ids)?;
    edges.retain(|edge| {
        kinds.contains(&edge.kind)
            && seen.insert((edge.target_graph_key.clone(), edge.target_node_id.clone()))
    });
    Ok(edges)
}

/// The edge kinds a callee list follows.
pub(in crate::mcp::tools) const CALL_KINDS: &[EdgeKind] =
    &[EdgeKind::Calls, EdgeKind::Instantiates];

//! GoFrame route-to-controller dispatch synthesis.

use std::collections::{BTreeMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::edges::{edge_meta, synthesized_edge};
use crate::db::QueryBuilder;
use crate::error::Result;
use crate::types::{Edge, Language, Node, NodeKind};

const GOFRAME_ROUTE_MARKER: &str = "::goframe-route:";
const FANOUT_CAP: usize = 2_000;

static POINTER_PARAM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\*\s*(?:(\w+)\.)?([A-Z]\w*)\b").expect("valid Go pointer parameter regex")
});

/// Pointer parameter types in qualified and bare form.
fn pointer_param_types(signature: &str) -> Vec<String> {
    let mut out = Vec::new();
    for captures in POINTER_PARAM_RE.captures_iter(signature) {
        let bare = captures[2].to_string();
        if let Some(package) = captures.get(1) {
            out.push(format!("{}.{}", package.as_str(), bare));
        }
        out.push(bare);
    }
    out
}

fn addon_root(path: &str) -> &str {
    let mut components = path.split(['/', '\\']);
    while let Some(component) = components.next() {
        if component == "addons" {
            return components.next().unwrap_or("");
        }
    }
    ""
}

/// Prefer controller-directory methods, then keep the handler in the same
/// addon module as the route. Remaining ambiguity is intentionally unresolved.
fn select_handler<'a>(candidates: &'a [Node], route_file: &str) -> Option<&'a Node> {
    if candidates.len() == 1 {
        return candidates.first();
    }

    let controller_candidates: Vec<&Node> = candidates
        .iter()
        .filter(|handler| {
            let path = handler.file_path.replace('\\', "/");
            path.contains("/controller/") || path.contains("/controllers/")
        })
        .collect();
    let candidates: Vec<&Node> = if controller_candidates.is_empty() {
        candidates.iter().collect()
    } else {
        controller_candidates
    };
    if candidates.len() == 1 {
        return candidates.first().copied();
    }

    let route_addon = addon_root(route_file);
    let same_module: Vec<&Node> = candidates
        .into_iter()
        .filter(|handler| addon_root(&handler.file_path) == route_addon)
        .collect();
    (same_module.len() == 1).then(|| same_module[0])
}

/// Join GoFrame route nodes to controller methods through the request type in
/// each method signature. GoFrame performs this binding reflectively, so static
/// call extraction cannot produce the edge.
pub(super) fn goframe_route_edges(queries: &QueryBuilder) -> Result<Vec<Edge>> {
    let mut routes_by_request: BTreeMap<String, Vec<Node>> = BTreeMap::new();
    let mut wanted = HashSet::new();
    queries.iterate_nodes_by_kind(NodeKind::Route, |route| {
        if route.language != Language::Go {
            return true;
        }
        let Some(marker) = route.qualified_name.find(GOFRAME_ROUTE_MARKER) else {
            return true;
        };
        let join_key = route.qualified_name[marker + GOFRAME_ROUTE_MARKER.len()..].to_string();
        if join_key.is_empty() {
            return true;
        }

        routes_by_request
            .entry(join_key.clone())
            .or_default()
            .push(route);
        wanted.insert(join_key.clone());
        if let Some((_, bare)) = join_key.rsplit_once('.') {
            wanted.insert(bare.to_string());
        }
        true
    })?;
    if routes_by_request.is_empty() {
        return Ok(Vec::new());
    }

    let mut handlers_by_key: BTreeMap<String, Vec<Node>> = BTreeMap::new();
    queries.iterate_nodes_by_kind(NodeKind::Method, |method| {
        if method.language != Language::Go {
            return true;
        }
        let Some(signature) = method.signature.as_deref() else {
            return true;
        };
        for parameter_type in pointer_param_types(signature) {
            if wanted.contains(&parameter_type) {
                handlers_by_key
                    .entry(parameter_type)
                    .or_default()
                    .push(method.clone());
            }
        }
        true
    })?;

    let mut edges = Vec::new();
    let mut seen = HashSet::new();
    let mut added = 0usize;
    for (join_key, routes) in routes_by_request {
        let bare = join_key
            .rsplit_once('.')
            .map_or(join_key.as_str(), |(_, bare)| bare);
        let candidates = handlers_by_key
            .get(&join_key)
            .or_else(|| handlers_by_key.get(bare));
        let Some(candidates) = candidates else {
            continue;
        };

        for route in routes {
            if added >= FANOUT_CAP {
                return Ok(edges);
            }
            let Some(handler) = select_handler(candidates, &route.file_path) else {
                continue;
            };
            if route.id == handler.id || !seen.insert(format!("{}>{}", route.id, handler.id)) {
                continue;
            }
            edges.push(synthesized_edge(
                &route.id,
                &handler.id,
                Some(route.start_line),
                edge_meta(vec![
                    ("synthesizedBy", Value::from("goframe-route")),
                    ("route", Value::from(route.name.as_str())),
                    ("requestType", Value::from(bare)),
                    (
                        "registeredAt",
                        Value::from(format!("{}:{}", handler.file_path, handler.start_line)),
                    ),
                ]),
            ));
            added += 1;
        }
    }
    Ok(edges)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::db::DatabaseConnection;

    fn method(id: &str, path: &str) -> Node {
        Node::new(
            id,
            NodeKind::Method,
            "List",
            "controller.List",
            path,
            Language::Go,
            10,
            20,
        )
    }

    #[test]
    fn extracts_qualified_and_bare_pointer_parameter_types() {
        assert_eq!(
            pointer_param_types(
                "func (c *Controller) List(ctx context.Context, req *cash.ListReq) (*cash.ListRes, error)"
            ),
            vec![
                "Controller",
                "cash.ListReq",
                "ListReq",
                "cash.ListRes",
                "ListRes"
            ]
        );
    }

    #[test]
    fn selects_handler_from_the_routes_addon() {
        let candidates = vec![
            method("core", "internal/controller/cash/list.go"),
            method("addon-a", "addons/a/internal/controller/cash/list.go"),
            method("addon-b", "addons/b/internal/controller/cash/list.go"),
        ];
        assert_eq!(
            select_handler(&candidates, "addons/b/api/cash/list.go").map(|node| node.id.as_str()),
            Some("addon-b")
        );
        assert_eq!(
            select_handler(&candidates, "api/cash/list.go").map(|node| node.id.as_str()),
            Some("core")
        );
    }

    #[test]
    fn leaves_cross_module_ambiguity_unresolved() {
        let candidates = vec![
            method("a", "addons/a/internal/controller/cash/list.go"),
            method("b", "addons/b/internal/controller/cash/list.go"),
        ];
        assert!(select_handler(&candidates, "api/cash/list.go").is_none());
    }

    #[test]
    fn joins_a_route_to_the_method_accepting_its_request_type() {
        let directory = tempdir().expect("temporary database directory");
        let connection = DatabaseConnection::initialize(directory.path().join("codegraph.db"))
            .expect("initialize database");
        let queries = QueryBuilder::new(connection.get_db().expect("database handle"));
        let route = Node::new(
            "route-list",
            NodeKind::Route,
            "GET /cash/list",
            "api/cash/list.go::goframe-route:cash.ListReq",
            "api/cash/list.go",
            Language::Go,
            3,
            3,
        );
        let mut handler = method("handler-list", "internal/controller/cash/list.go");
        handler.signature = Some(
            "func (controller *Controller) List(ctx context.Context, req *cash.ListReq) (*cash.ListRes, error)"
                .to_string(),
        );
        queries
            .insert_nodes(&[route, handler])
            .expect("insert fixture nodes");

        let edges = goframe_route_edges(&queries).expect("synthesize GoFrame route edge");
        let edge = edges
            .iter()
            .find(|edge| edge.source == "route-list" && edge.target == "handler-list")
            .expect("route reaches controller handler");
        assert_eq!(edge.line, Some(3));
        assert_eq!(
            edge.metadata
                .as_ref()
                .and_then(|metadata| metadata.get("requestType"))
                .and_then(Value::as_str),
            Some("ListReq")
        );
    }
}

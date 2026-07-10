//! Go receiver ownership and implicit-interface synthesis.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::edges::edge_meta;
use super::source::methods_of;
use crate::db::QueryBuilder;
use crate::error::Result;
use crate::types::{Edge, EdgeKind, Language, NodeKind, Provenance};

const MAX_CALLBACKS_PER_CHANNEL: usize = 40;

fn is_go_type_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Struct
            | NodeKind::Class
            | NodeKind::Interface
            | NodeKind::Enum
            | NodeKind::TypeAlias
    )
}

fn directory(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    normalized
        .rsplit_once('/')
        .map_or_else(String::new, |(dir, _)| dir.to_string())
}

fn method_names(queries: &QueryBuilder, owner_id: &str) -> Result<HashSet<String>> {
    Ok(methods_of(queries, owner_id)?
        .into_iter()
        .map(|method| method.name)
        .collect())
}

/// Attach orphaned Go receiver methods to a same-package type declaration.
///
/// Extraction can only attach a receiver method when its type is in the same
/// file. Go requires both declarations to live in the same package, represented
/// here by their directory, so the cross-file link is deterministic.
pub(super) fn go_cross_file_method_contains_edges(queries: &QueryBuilder) -> Result<Vec<Edge>> {
    let mut methods = Vec::new();
    queries.iterate_nodes_by_kind(NodeKind::Method, |method| {
        if method.language == Language::Go {
            methods.push(method);
        }
        true
    })?;

    let mut edges = Vec::new();
    let mut seen = HashSet::new();
    for method in methods {
        let Some((receiver, _)) = method.qualified_name.rsplit_once("::") else {
            continue;
        };
        if receiver.is_empty() {
            continue;
        }

        let mut has_type_parent = false;
        for incoming in queries.get_incoming_edges(&method.id, Some(&[EdgeKind::Contains]))? {
            if queries
                .get_node_by_id(&incoming.source)?
                .is_some_and(|node| is_go_type_kind(node.kind))
            {
                has_type_parent = true;
                break;
            }
        }
        if has_type_parent {
            continue;
        }

        let method_dir = directory(&method.file_path);
        let owner = queries
            .get_nodes_by_name(receiver)?
            .into_iter()
            .find(|node| {
                node.language == Language::Go
                    && is_go_type_kind(node.kind)
                    && directory(&node.file_path) == method_dir
            });
        let Some(owner) = owner else {
            continue;
        };

        if !seen.insert(format!("{}>{}", owner.id, method.id)) {
            continue;
        }
        let mut edge = Edge::new(owner.id, method.id, EdgeKind::Contains);
        edge.line = Some(method.start_line);
        edges.push(edge);
    }
    Ok(edges)
}

/// Synthesize Go's implicit struct-to-interface relationships by method set.
///
/// Go has no explicit `implements` declaration. Name-only method-set matching
/// intentionally over-approximates the relationship, matching the dispatch
/// synthesizer's existing policy. Empty interfaces are skipped.
pub(super) fn go_implements_edges(queries: &QueryBuilder) -> Result<Vec<Edge>> {
    let mut structs = Vec::new();
    queries.iterate_nodes_by_kind(NodeKind::Struct, |node| {
        if node.language == Language::Go {
            structs.push(node);
        }
        true
    })?;

    let mut struct_methods = HashMap::new();
    for node in &structs {
        struct_methods.insert(node.id.clone(), method_names(queries, &node.id)?);
    }

    let mut interfaces = Vec::new();
    queries.iterate_nodes_by_kind(NodeKind::Interface, |node| {
        if node.language == Language::Go {
            interfaces.push(node);
        }
        true
    })?;

    let mut edges = Vec::new();
    let mut seen = HashSet::new();
    for interface in interfaces {
        let wanted = method_names(queries, &interface.id)?;
        if wanted.is_empty() {
            continue;
        }

        let mut added = 0usize;
        for implementor in &structs {
            if added >= MAX_CALLBACKS_PER_CHANNEL {
                break;
            }
            let Some(have) = struct_methods.get(&implementor.id) else {
                continue;
            };
            if have.len() < wanted.len() || !wanted.iter().all(|name| have.contains(name)) {
                continue;
            }
            if !seen.insert(format!("{}>{}", implementor.id, interface.id)) {
                continue;
            }

            edges.push(Edge {
                source: implementor.id.clone(),
                target: interface.id.clone(),
                kind: EdgeKind::Implements,
                metadata: Some(edge_meta(vec![
                    ("synthesizedBy", Value::from("go-implements")),
                    ("via", Value::from(interface.name.as_str())),
                    (
                        "registeredAt",
                        Value::from(format!(
                            "{}:{}",
                            implementor.file_path, implementor.start_line
                        )),
                    ),
                ])),
                line: Some(implementor.start_line),
                column: None,
                provenance: Some(Provenance::Heuristic),
            });
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
    use crate::types::Node;

    fn node(
        id: &str,
        kind: NodeKind,
        name: &str,
        qualified_name: &str,
        file: &str,
        line: u32,
    ) -> Node {
        Node::new(
            id,
            kind,
            name,
            qualified_name,
            file,
            Language::Go,
            line,
            line + 1,
        )
    }

    #[test]
    fn attaches_cross_file_method_only_to_same_package_receiver() {
        let dir = tempdir().unwrap();
        let connection = DatabaseConnection::initialize(dir.path().join("codegraph.db")).unwrap();
        let queries = QueryBuilder::new(connection.get_db().unwrap());
        queries
            .insert_nodes(&[
                node("user", NodeKind::Struct, "User", "User", "pkg/user.go", 3),
                node(
                    "other-user",
                    NodeKind::Struct,
                    "User",
                    "User",
                    "other/user.go",
                    3,
                ),
                node(
                    "save",
                    NodeKind::Method,
                    "Save",
                    "User::Save",
                    "pkg/store.go",
                    8,
                ),
            ])
            .unwrap();

        let edges = go_cross_file_method_contains_edges(&queries).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].source, "user");
        assert_eq!(edges[0].target, "save");
        assert_eq!(edges[0].kind, EdgeKind::Contains);
        assert_eq!(edges[0].provenance, None);
    }

    #[test]
    fn skips_method_that_already_has_a_type_parent() {
        let dir = tempdir().unwrap();
        let connection = DatabaseConnection::initialize(dir.path().join("codegraph.db")).unwrap();
        let queries = QueryBuilder::new(connection.get_db().unwrap());
        queries
            .insert_nodes(&[
                node("user", NodeKind::Struct, "User", "User", "pkg/user.go", 3),
                node(
                    "save",
                    NodeKind::Method,
                    "Save",
                    "User::Save",
                    "pkg/store.go",
                    8,
                ),
            ])
            .unwrap();
        queries
            .insert_edges(&[Edge::new("user", "save", EdgeKind::Contains)])
            .unwrap();

        assert!(
            go_cross_file_method_contains_edges(&queries)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn matches_non_empty_interface_method_set() {
        let dir = tempdir().unwrap();
        let connection = DatabaseConnection::initialize(dir.path().join("codegraph.db")).unwrap();
        let queries = QueryBuilder::new(connection.get_db().unwrap());
        queries
            .insert_nodes(&[
                node(
                    "service",
                    NodeKind::Struct,
                    "Service",
                    "Service",
                    "pkg/service.go",
                    2,
                ),
                node(
                    "iface",
                    NodeKind::Interface,
                    "Reader",
                    "Reader",
                    "pkg/api.go",
                    2,
                ),
                node("empty", NodeKind::Interface, "Any", "Any", "pkg/api.go", 10),
                node(
                    "read-impl",
                    NodeKind::Method,
                    "Read",
                    "Service::Read",
                    "pkg/service.go",
                    4,
                ),
                node(
                    "read-decl",
                    NodeKind::Method,
                    "Read",
                    "Reader::Read",
                    "pkg/api.go",
                    4,
                ),
            ])
            .unwrap();
        queries
            .insert_edges(&[
                Edge::new("service", "read-impl", EdgeKind::Contains),
                Edge::new("iface", "read-decl", EdgeKind::Contains),
            ])
            .unwrap();

        let edges = go_implements_edges(&queries).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].source, "service");
        assert_eq!(edges[0].target, "iface");
        assert_eq!(edges[0].kind, EdgeKind::Implements);
        assert_eq!(edges[0].provenance, Some(Provenance::Heuristic));
        assert_eq!(
            edges[0].metadata.as_ref().unwrap()["synthesizedBy"],
            "go-implements"
        );
    }
}

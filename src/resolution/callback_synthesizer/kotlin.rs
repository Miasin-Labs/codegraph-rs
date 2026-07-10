//! Kotlin Multiplatform `expect`/`actual` edge synthesis.

use std::collections::HashSet;

use serde_json::Value;

use super::edges::{edge_meta, synthesized_edge};
use crate::db::QueryBuilder;
use crate::error::Result;
use crate::types::{Edge, Language, NODE_KINDS, Node, NodeKind};

const MAX_CALLBACKS_PER_CHANNEL: usize = 40;

fn has_modifier(node: &Node, modifier: &str) -> bool {
    node.decorators
        .as_ref()
        .is_some_and(|decorators| decorators.iter().any(|item| item == modifier))
}

fn is_kmp_type_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class
            | NodeKind::Interface
            | NodeKind::Struct
            | NodeKind::Enum
            | NodeKind::TypeAlias
    )
}

fn kmp_kinds_compatible(declaration: NodeKind, implementation: NodeKind) -> bool {
    declaration == implementation
        || (is_kmp_type_kind(declaration) && is_kmp_type_kind(implementation))
}

fn append_actual_edges(
    actual: &Node,
    candidates: &[Node],
    seen: &mut HashSet<String>,
    edges: &mut Vec<Edge>,
) {
    let mut added = 0usize;
    for declaration in candidates {
        if added >= MAX_CALLBACKS_PER_CHANNEL {
            break;
        }
        if declaration.language != Language::Kotlin || declaration.id == actual.id {
            continue;
        }
        if !kmp_kinds_compatible(declaration.kind, actual.kind)
            || declaration.file_path == actual.file_path
            || has_modifier(declaration, "actual")
        {
            continue;
        }

        let key = format!("{}>{}", declaration.id, actual.id);
        if !seen.insert(key) {
            continue;
        }
        edges.push(synthesized_edge(
            &declaration.id,
            &actual.id,
            Some(declaration.start_line),
            edge_meta(vec![
                ("synthesizedBy", Value::from("kotlin-expect-actual")),
                ("via", Value::from(actual.name.as_str())),
                (
                    "registeredAt",
                    Value::from(format!("{}:{}", actual.file_path, actual.start_line)),
                ),
            ]),
        ));
        added += 1;
    }
}

/// Link a common-source-set Kotlin declaration to each platform `actual`
/// implementation with the same fully-qualified name.
///
/// Members of an `expect class` are not themselves marked `expect`, so the
/// declaration side is deliberately any non-`actual` Kotlin node. Requiring an
/// `actual` counterpart, a different file, and an exact qualified name keeps
/// plain cross-file overloads out. Type-like declarations are compatible with
/// each other because Kotlin commonly fulfills `expect class` with an
/// `actual typealias`.
pub(super) fn kotlin_expect_actual_edges(queries: &QueryBuilder) -> Result<Vec<Edge>> {
    // Stream the indexed kind buckets and retain only `actual` declarations.
    // This avoids hydrating the full node table on large graphs while requiring
    // no special-purpose query API.
    let mut actuals = Vec::new();
    for kind in NODE_KINDS {
        queries.iterate_nodes_by_kind(kind, |node| {
            if node.language == Language::Kotlin && has_modifier(&node, "actual") {
                actuals.push(node);
            }
            true
        })?;
    }

    let mut edges = Vec::new();
    let mut seen = HashSet::new();
    for actual in actuals {
        let candidates = queries.get_nodes_by_qualified_name_exact(&actual.qualified_name)?;
        append_actual_edges(&actual, &candidates, &mut seen, &mut edges);
    }
    Ok(edges)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{EdgeKind, Provenance};

    fn node(
        id: &str,
        kind: NodeKind,
        qualified_name: &str,
        file_path: &str,
        modifier: Option<&str>,
    ) -> Node {
        let name = qualified_name.rsplit('.').next().unwrap_or(qualified_name);
        let mut node = Node::new(
            id,
            kind,
            name,
            qualified_name,
            file_path,
            Language::Kotlin,
            7,
            9,
        );
        node.decorators = modifier.map(|value| vec![value.to_string()]);
        node
    }

    #[test]
    fn links_common_declaration_to_actual_implementation() {
        let declaration = node(
            "common",
            NodeKind::Function,
            "com.example.fetch",
            "src/commonMain/Api.kt",
            Some("expect"),
        );
        let actual = node(
            "jvm",
            NodeKind::Function,
            "com.example.fetch",
            "src/jvmMain/Api.kt",
            Some("actual"),
        );
        let mut edges = Vec::new();
        append_actual_edges(&actual, &[declaration], &mut HashSet::new(), &mut edges);

        assert_eq!(edges.len(), 1);
        let edge = &edges[0];
        assert_eq!(edge.source, "common");
        assert_eq!(edge.target, "jvm");
        assert_eq!(edge.kind, EdgeKind::Calls);
        assert_eq!(edge.line, Some(7));
        assert_eq!(edge.provenance, Some(Provenance::Heuristic));
        let metadata = edge.metadata.as_ref().unwrap();
        assert_eq!(metadata["synthesizedBy"], "kotlin-expect-actual");
        assert_eq!(metadata["via"], "fetch");
        assert_eq!(metadata["registeredAt"], "src/jvmMain/Api.kt:7");
    }

    #[test]
    fn permits_expect_class_fulfilled_by_actual_typealias() {
        let declaration = node(
            "common",
            NodeKind::Class,
            "com.example.Clock",
            "src/commonMain/Clock.kt",
            Some("expect"),
        );
        let actual = node(
            "native",
            NodeKind::TypeAlias,
            "com.example.Clock",
            "src/nativeMain/Clock.kt",
            Some("actual"),
        );
        let mut edges = Vec::new();
        append_actual_edges(&actual, &[declaration], &mut HashSet::new(), &mut edges);
        assert_eq!(edges.len(), 1);
    }

    #[test]
    fn excludes_actual_siblings_same_file_and_incompatible_kinds() {
        let actual = node(
            "jvm",
            NodeKind::Function,
            "com.example.fetch",
            "src/jvmMain/Api.kt",
            Some("actual"),
        );
        let sibling = node(
            "native",
            NodeKind::Function,
            "com.example.fetch",
            "src/nativeMain/Api.kt",
            Some("actual"),
        );
        let same_file = node(
            "same-file",
            NodeKind::Function,
            "com.example.fetch",
            "src/jvmMain/Api.kt",
            None,
        );
        let incompatible = node(
            "class",
            NodeKind::Class,
            "com.example.fetch",
            "src/commonMain/Api.kt",
            Some("expect"),
        );
        let mut edges = Vec::new();
        append_actual_edges(
            &actual,
            &[sibling, same_file, incompatible],
            &mut HashSet::new(),
            &mut edges,
        );
        assert!(edges.is_empty());
    }

    #[test]
    fn unmarked_expect_class_member_is_still_a_declaration() {
        let declaration = node(
            "common-member",
            NodeKind::Method,
            "com.example.Clock.now",
            "src/commonMain/Clock.kt",
            None,
        );
        let actual = node(
            "jvm-member",
            NodeKind::Method,
            "com.example.Clock.now",
            "src/jvmMain/Clock.kt",
            Some("actual"),
        );
        let mut edges = Vec::new();
        append_actual_edges(&actual, &[declaration], &mut HashSet::new(), &mut edges);
        assert_eq!(edges.len(), 1);
    }
}

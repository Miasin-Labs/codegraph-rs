//! Expo Modules and classic React Native cross-platform method pairing.

use std::collections::{BTreeMap, HashSet};

use serde_json::Value;

use super::edges::{edge_meta, synthesized_edge};
use crate::db::QueryBuilder;
use crate::error::Result;
use crate::types::{Edge, EdgeKind, Node, NodeKind};

const RN_INFRASTRUCTURE_METHODS: &[&str] = &[
    "addListener",
    "removeListeners",
    "getConstants",
    "constantsToExport",
    "getName",
    "invalidate",
    "initialize",
    "getDefaultEventTypes",
    "supportedEvents",
    "requiresMainQueueSetup",
    "methodQueue",
];

fn collect_methods(queries: &QueryBuilder) -> Result<Vec<Node>> {
    let mut methods = Vec::new();
    queries.iterate_nodes_by_kind(NodeKind::Method, |node| {
        methods.push(node);
        true
    })?;
    Ok(methods)
}

fn expo_pairs(methods: impl IntoIterator<Item = Node>) -> Vec<Edge> {
    let mut by_key: BTreeMap<String, Vec<Node>> = BTreeMap::new();
    for method in methods {
        if !method.id.starts_with("expo-module:") {
            continue;
        }
        let Some(key) = method.qualified_name.rsplit("::").next() else {
            continue;
        };
        if key.is_empty() {
            continue;
        }
        by_key.entry(key.to_string()).or_default().push(method);
    }

    let mut edges = Vec::new();
    let mut seen = HashSet::new();
    for group in by_key.into_values().filter(|group| group.len() >= 2) {
        for source in &group {
            for target in &group {
                if source.id == target.id || source.language == target.language {
                    continue;
                }
                if !seen.insert(format!("{}>{}", source.id, target.id)) {
                    continue;
                }
                edges.push(synthesized_edge(
                    &source.id,
                    &target.id,
                    Some(source.start_line),
                    edge_meta(vec![
                        ("synthesizedBy", Value::from("expo-cross-platform")),
                        ("via", Value::from(source.name.as_str())),
                    ]),
                ));
            }
        }
    }
    edges
}

/// Pair the same Expo Modules API implementation across native languages.
pub(super) fn expo_cross_platform_edges(queries: &QueryBuilder) -> Result<Vec<Edge>> {
    Ok(expo_pairs(collect_methods(queries)?))
}

fn normalized_rn_method(name: &str) -> &str {
    name.split(':').next().unwrap_or(name)
}

fn is_rn_native_language(node: &Node) -> bool {
    matches!(node.language.as_str(), "java" | "kotlin" | "objc" | "cpp")
}

fn is_js_language(node: &Node) -> bool {
    matches!(
        node.language.as_str(),
        "typescript" | "tsx" | "javascript" | "jsx"
    )
}

fn rn_pairs(
    methods: impl IntoIterator<Item = Node>,
    confirmed_bridges: &HashSet<String>,
) -> Vec<Edge> {
    let mut by_name: BTreeMap<String, Vec<Node>> = BTreeMap::new();
    for method in methods {
        if !is_rn_native_language(&method) {
            continue;
        }
        by_name
            .entry(normalized_rn_method(&method.name).to_string())
            .or_default()
            .push(method);
    }

    let infrastructure = RN_INFRASTRUCTURE_METHODS
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    let mut edges = Vec::new();
    let mut seen = HashSet::new();
    for (name, group) in by_name {
        if infrastructure.contains(name.as_str()) {
            continue;
        }
        let languages = group
            .iter()
            .map(|method| method.language.as_str())
            .collect::<HashSet<_>>();
        if languages.len() < 2 {
            continue;
        }
        for bridge in group
            .iter()
            .filter(|method| confirmed_bridges.contains(&method.id))
        {
            for sibling in &group {
                if sibling.id == bridge.id || sibling.language == bridge.language {
                    continue;
                }
                for (source, target) in [(bridge, sibling), (sibling, bridge)] {
                    if !seen.insert(format!("{}>{}", source.id, target.id)) {
                        continue;
                    }
                    edges.push(synthesized_edge(
                        &source.id,
                        &target.id,
                        Some(source.start_line),
                        edge_meta(vec![
                            ("synthesizedBy", Value::from("rn-cross-platform")),
                            ("via", Value::from(name.as_str())),
                        ]),
                    ));
                }
            }
        }
    }
    edges
}

/// Pair classic React Native native-module implementations only when a
/// JS-family call edge confirms that one member is a bridge method.
pub(super) fn rn_cross_platform_edges(queries: &QueryBuilder) -> Result<Vec<Edge>> {
    let methods = collect_methods(queries)?;
    let mut confirmed = HashSet::new();
    for method in methods
        .iter()
        .filter(|method| is_rn_native_language(method))
    {
        let incoming = queries.get_incoming_edges(&method.id, Some(&[EdgeKind::Calls]))?;
        if incoming.is_empty() {
            continue;
        }
        let source_ids = incoming
            .iter()
            .map(|edge| edge.source.clone())
            .collect::<Vec<_>>();
        let sources = queries.get_nodes_by_ids(&source_ids)?;
        if incoming
            .iter()
            .any(|edge| sources.get(&edge.source).is_some_and(is_js_language))
        {
            confirmed.insert(method.id.clone());
        }
    }
    Ok(rn_pairs(methods, &confirmed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Language, NodeKind};

    fn method(id: &str, name: &str, qualified: &str, language: Language) -> Node {
        Node::new(
            id,
            NodeKind::Method,
            name,
            qualified,
            format!("{id}.src"),
            language,
            3,
            8,
        )
    }

    #[test]
    fn expo_pairs_only_equal_module_method_keys_across_languages() {
        let methods = vec![
            method(
                "expo-module:ios",
                "status",
                "ios.swift::Battery.status",
                Language::Swift,
            ),
            method(
                "expo-module:android",
                "status",
                "android.kt::Battery.status",
                Language::Kotlin,
            ),
            method(
                "expo-module:other",
                "status",
                "other.kt::Network.status",
                Language::Kotlin,
            ),
        ];
        let edges = expo_pairs(methods);
        assert_eq!(edges.len(), 2);
        assert!(edges.iter().any(|edge| {
            edge.source == "expo-module:ios" && edge.target == "expo-module:android"
        }));
    }

    #[test]
    fn rn_pairing_requires_a_confirmed_js_bridge_and_skips_infrastructure() {
        let methods = vec![
            method("android", "read", "Disk.read", Language::Java),
            method("ios", "read:", "Disk.read:", Language::Objc),
            method("android-name", "getName", "Disk.getName", Language::Java),
            method("ios-name", "getName", "Disk.getName", Language::Objc),
        ];
        assert!(rn_pairs(methods.clone(), &HashSet::new()).is_empty());

        let confirmed = HashSet::from(["android".to_string(), "android-name".to_string()]);
        let edges = rn_pairs(methods, &confirmed);
        assert_eq!(edges.len(), 2);
        assert!(
            edges
                .iter()
                .any(|edge| edge.source == "android" && edge.target == "ios")
        );
        assert!(
            edges
                .iter()
                .all(|edge| !edge.source.contains("name") && !edge.target.contains("name"))
        );
    }
}

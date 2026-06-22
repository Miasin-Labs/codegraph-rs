//! React Native Fabric component-to-native implementation synthesis.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::edges::{edge_meta, synthesized_edge};
use crate::resolution::types::ResolutionContext;
use crate::types::{Edge, Language, Node, NodeKind};

/// Phase 6 — React Native Fabric/Codegen view component bridge.
///
/// The Fabric framework extractor (`frameworks/fabric.ts`) emits
/// `component` nodes named after the JS-visible component (e.g.
/// `RNSScreenStack`) from each `codegenNativeComponent<Props>('Name')`
/// spec declaration. The native implementation lives in an ObjC++/.mm or
/// Kotlin/Java class whose name follows one of RN's conventions:
///
///   - Exact: `RNSScreenStack`
///   - With suffix: `RNSScreenStackView`, `RNSScreenStackViewManager`,
///     `RNSScreenStackComponentView`, `RNSScreenStackManager`
///
/// This synthesizer walks every Fabric component node and looks for a
/// native class matching one of those names; when found, emits a
/// `calls` edge `component → native class` (provenance `'heuristic'`,
/// `synthesizedBy:'fabric-native-impl'`) so trace from JSX usage of the
/// component continues into native.
///
/// The convention-based suffix lookup is precise: there's no name
/// collision in RN view-manager codebases by design (Codegen output would
/// conflict otherwise).
const FABRIC_NATIVE_SUFFIXES: [&str; 5] = ["", "View", "ViewManager", "ComponentView", "Manager"];

pub(super) fn fabric_native_impl_edges(ctx: &dyn ResolutionContext) -> Vec<Edge> {
    let mut edges: Vec<Edge> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // The Fabric extractor IDs are prefixed `fabric-component:` so we can
    // filter to just those without iterating all `component` nodes.
    let components: Vec<Node> = ctx
        .get_nodes_by_kind(NodeKind::Component)
        .into_iter()
        .filter(|n| n.id.starts_with("fabric-component:"))
        .collect();
    if components.is_empty() {
        return edges;
    }

    // Pre-index native classes by name for O(1) lookup.
    let mut native_classes_by_name: HashMap<String, Vec<Node>> = HashMap::new();
    for n in ctx.get_nodes_by_kind(NodeKind::Class) {
        if n.language != Language::Objc
            && n.language != Language::Kotlin
            && n.language != Language::Java
            && n.language != Language::Cpp
        {
            continue;
        }
        native_classes_by_name
            .entry(n.name.clone())
            .or_default()
            .push(n);
    }

    for component in &components {
        for suffix in FABRIC_NATIVE_SUFFIXES {
            let candidate = format!("{}{}", component.name, suffix);
            let Some(matches) = native_classes_by_name.get(&candidate) else {
                continue;
            };
            if matches.is_empty() {
                continue;
            }
            // Link the component node to every matching native class (iOS +
            // Android each have one).
            for native in matches {
                let key = format!("{}>{}", component.id, native.id);
                if !seen.insert(key) {
                    continue;
                }
                edges.push(synthesized_edge(
                    &component.id,
                    &native.id,
                    None,
                    edge_meta(vec![
                        ("synthesizedBy", Value::from("fabric-native-impl")),
                        (
                            "viaSuffix",
                            Value::from(if suffix.is_empty() { "(exact)" } else { suffix }),
                        ),
                        ("componentName", Value::from(component.name.as_str())),
                    ]),
                ));
            }
        }
    }

    edges
}

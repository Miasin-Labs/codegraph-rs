//! Vue SFC template synthesis.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::edges::{edge_meta, synthesized_edge};
use super::source::kebab_to_pascal;
use crate::resolution::types::ResolutionContext;
use crate::types::{Edge, Metadata, Node, NodeKind};

const MAX_JSX_CHILDREN: usize = 30;
static VUE_KEBAB_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<([a-z][a-z0-9]*(?:-[a-z0-9]+)+)[\s/>]").expect("valid regex"));
static VUE_HANDLER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:@|v-on:)([a-zA-Z][0-9A-Za-z_-]*)(?:\.[0-9A-Za-z_]+)*\s*=\s*"([^"]+)""#)
        .expect("valid regex")
});
static VUE_DESTRUCTURE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:const|let|var)\s*\{([^}]+)\}\s*=\s*([0-9A-Za-z_]+)\s*\(").expect("valid regex")
});

static VUE_TEMPLATE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<template[^>]*>([\s\S]*)</template>").expect("valid regex"));
static VUE_SCRIPT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<script[^>]*>([\s\S]*?)</script>").expect("valid regex"));
static VUE_USE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^use[A-Z]").expect("valid regex"));
static VUE_DESTRUCTURE_PART_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([0-9A-Za-z_]+)\s*(?::\s*([0-9A-Za-z_]+))?$").expect("valid regex")
});
static VUE_HANDLER_NAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Za-z_][0-9A-Za-z_]*)").expect("valid regex"));

/// Phase 6: Vue SFC templates. The `.vue` extractor only parses `<script>`, so
/// template usage is invisible — child components and event handlers used ONLY in
/// the template have no edge to them. PascalCase children (`<VPNav/>`) are already
/// caught by reactJsxChildEdges (which scans the SFC component node), so this adds
/// the two Vue-specific shapes:
///   - kebab-case children: `<el-button>` → `ElButton` component (renders).
///   - event bindings: `@click="onClick"` / `v-on:submit="save"` → handler method.
///
/// Scoped to the `<template>` block of `.vue` files; resolution gate (kebab→
/// component, handler→function/method) keeps precision; inline arrows / `$emit`
/// skipped.
pub(super) fn vue_template_edges(ctx: &dyn ResolutionContext) -> Vec<Edge> {
    let mut edges: Vec<Edge> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let component_kinds = [NodeKind::Component, NodeKind::Function, NodeKind::Class];
    let handler_kinds = [NodeKind::Method, NodeKind::Function];
    // A composable's returned member may be a fn (`function close(){}`) or an
    // arrow assigned to a const (`const close = () => {}`).
    let return_kinds = [
        NodeKind::Method,
        NodeKind::Function,
        NodeKind::Variable,
        NodeKind::Constant,
    ];
    for file in ctx.get_all_files() {
        if !file.ends_with(".vue") {
            continue;
        }
        let Some(content) = ctx.read_file(&file) else {
            continue;
        };
        if content.is_empty() {
            continue;
        }
        let tpl = VUE_TEMPLATE_RE
            .captures(&content)
            .and_then(|c| c.get(1))
            .map(|g| g.as_str().to_string());
        let Some(tpl) = tpl else { continue };
        if tpl.is_empty() {
            continue;
        }
        let comp = ctx
            .get_nodes_in_file(&file)
            .into_iter()
            .find(|n| n.kind == NodeKind::Component);
        let Some(comp) = comp else { continue };

        // Composable-destructure map: alias → (composable, key). Lets us resolve a
        // template handler that isn't a local function but a destructured composable
        // return (`@click="closeSidebar"` ← `const { close: closeSidebar } = useSidebarControl()`).
        let script = VUE_SCRIPT_RE
            .captures(&content)
            .and_then(|c| c.get(1))
            .map(|g| g.as_str().to_string())
            .unwrap_or_default();
        let mut destructured: HashMap<String, (String, String)> = HashMap::new();
        for dm in VUE_DESTRUCTURE_RE.captures_iter(&script) {
            if !VUE_USE_RE.is_match(&dm[2]) {
                continue; // composables / hooks only
            }
            for part in dm[1].split(',') {
                // key | key: alias
                if let Some(pm) = VUE_DESTRUCTURE_PART_RE.captures(part.trim()) {
                    let alias = pm
                        .get(2)
                        .map(|g| g.as_str())
                        .unwrap_or_else(|| pm.get(1).expect("group 1").as_str());
                    destructured.insert(alias.to_string(), (dm[2].to_string(), pm[1].to_string()));
                }
            }
        }

        let mut added = 0usize;
        let add_edge = |target: Option<&Node>,
                        meta: Metadata,
                        edges: &mut Vec<Edge>,
                        seen: &mut HashSet<String>,
                        added: &mut usize| {
            let Some(target) = target else { return };
            if *added >= MAX_JSX_CHILDREN || target.id == comp.id {
                return;
            }
            let synthesized_by = meta
                .get("synthesizedBy")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let k = format!("{}>{}>{}", comp.id, target.id, synthesized_by);
            if !seen.insert(k) {
                return;
            }
            edges.push(synthesized_edge(
                &comp.id,
                &target.id,
                Some(comp.start_line),
                meta,
            ));
            *added += 1;
        };
        // Prefer a target in THIS SFC (handlers live in the same file's script) —
        // avoids cross-file mis-match when a name repeats across a monorepo.
        let resolve = |name: &str, kinds: &[NodeKind]| -> Option<Node> {
            let matches: Vec<Node> = ctx
                .get_nodes_by_name(name)
                .into_iter()
                .filter(|n| kinds.contains(&n.kind))
                .collect();
            matches
                .iter()
                .find(|n| n.file_path == file)
                .cloned()
                .or_else(|| matches.into_iter().next())
        };

        for m in VUE_KEBAB_RE.captures_iter(&tpl) {
            let target = resolve(&kebab_to_pascal(&m[1]), &component_kinds);
            add_edge(
                target.as_ref(),
                edge_meta(vec![
                    ("synthesizedBy", Value::from("jsx-render")),
                    ("via", Value::from(&m[1])),
                ]),
                &mut edges,
                &mut seen,
                &mut added,
            );
        }
        for m in VUE_HANDLER_RE.captures_iter(&tpl) {
            let event = m[1].to_string();
            let expr = m[2].trim().to_string();
            if expr.contains("=>") || expr.starts_with('$') {
                continue; // inline arrow / $emit
            }
            let Some(nm) = VUE_HANDLER_NAME_RE.captures(&expr) else {
                continue;
            };
            let name = nm[1].to_string();
            if let Some(direct) = resolve(&name, &handler_kinds) {
                add_edge(
                    Some(&direct),
                    edge_meta(vec![
                        ("synthesizedBy", Value::from("vue-handler")),
                        ("event", Value::from(event.as_str())),
                    ]),
                    &mut edges,
                    &mut seen,
                    &mut added,
                );
                continue;
            }
            // Composable-destructure handler → resolve to the composable's returned fn.
            let Some((composable_name, key)) = destructured.get(&name) else {
                continue;
            };
            let composable = resolve(composable_name, &handler_kinds);
            // Resolve to the SPECIFIC returned member (e.g. `close`) defined in the
            // composable's file. No fallback to the composable itself — the component
            // already has a static `useX()` call edge, so that would just be redundant
            // and less precise.
            let key_fn = composable.as_ref().and_then(|c| {
                ctx.get_nodes_by_name(key)
                    .into_iter()
                    .find(|n| return_kinds.contains(&n.kind) && n.file_path == c.file_path)
            });
            if let Some(key_fn) = key_fn {
                add_edge(
                    Some(&key_fn),
                    edge_meta(vec![
                        ("synthesizedBy", Value::from("vue-handler")),
                        ("event", Value::from(event.as_str())),
                        ("via", Value::from(composable_name.as_str())),
                    ]),
                    &mut edges,
                    &mut seen,
                    &mut added,
                );
            }
        }
    }
    edges
}

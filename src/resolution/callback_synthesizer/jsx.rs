//! React JSX child-render synthesis.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::edges::{edge_meta, synthesized_edge};
use super::ordered::OrderedSet;
use super::source::{is_fn_kind, slice_lines};
use crate::resolution::types::ResolutionContext;
use crate::types::{Edge, Node, NodeKind};

const MAX_JSX_CHILDREN: usize = 30;
static JSX_TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<([A-Z][A-Za-z0-9_]*)[\s/>]").expect("valid regex"));

/// Phase 5: React JSX child rendering. A component that returns `<Child .../>`
/// mounts Child — React calls it — but JSX instantiation isn't a static call edge,
/// so a render tree (App.render → StaticCanvas → renderStaticScene) breaks at the
/// JSX hop. Link parent → each capitalized JSX child it renders. File-oriented
/// (read each JSX file once). Precision gate: the child name must resolve to a
/// component/function/class node — TS generics like `Array<Foo>` resolve to a type
/// (or nothing) and are dropped.
pub(super) fn react_jsx_child_edges(ctx: &dyn ResolutionContext) -> Vec<Edge> {
    let mut edges: Vec<Edge> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for file in ctx.get_all_files() {
        let Some(content) = ctx.read_file(&file) else {
            continue;
        };
        if content.is_empty() || (!content.contains("</") && !content.contains("/>")) {
            continue; // JSX-file gate
        }
        let parents: Vec<Node> = ctx
            .get_nodes_in_file(&file)
            .into_iter()
            .filter(|n| is_fn_kind(n.kind))
            .collect();
        for parent in &parents {
            let Some(src) = slice_lines(&content, parent.start_line, parent.end_line) else {
                continue;
            };
            if src.is_empty() || (!src.contains("</") && !src.contains("/>")) {
                continue;
            }
            let mut names = OrderedSet::default();
            for m in JSX_TAG_RE.captures_iter(&src) {
                names.add(&m[1]);
            }
            let mut added = 0usize;
            for name in names.iter() {
                if added >= MAX_JSX_CHILDREN {
                    break;
                }
                let child = ctx.get_nodes_by_name(name).into_iter().find(|n| {
                    n.kind == NodeKind::Component
                        || n.kind == NodeKind::Function
                        || n.kind == NodeKind::Class
                });
                let Some(child) = child else { continue };
                if child.id == parent.id {
                    continue;
                }
                let key = format!("{}>{}", parent.id, child.id);
                if !seen.insert(key) {
                    continue;
                }
                edges.push(synthesized_edge(
                    &parent.id,
                    &child.id,
                    Some(parent.start_line),
                    edge_meta(vec![
                        ("synthesizedBy", Value::from("jsx-render")),
                        ("via", Value::from(name.as_str())),
                    ]),
                ));
                added += 1;
            }
        }
    }
    edges
}

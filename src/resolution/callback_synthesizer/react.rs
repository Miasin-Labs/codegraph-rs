//! React class-component render synthesis.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::edges::{edge_meta, synthesized_edge};
use super::source::{methods_of, node_source};
use crate::db::QueryBuilder;
use crate::error::Result;
use crate::resolution::types::ResolutionContext;
use crate::types::{Edge, NodeKind};

const MAX_CALLBACKS_PER_CHANNEL: usize = 40;
static SETSTATE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"this\.setState\s*\(").expect("valid regex"));

/// Phase 4: React class-component re-render. `this.setState(...)` re-runs the
/// component's `render()`, but that hop is React-internal — no static edge — so a
/// flow like "mutation → setState → canvas repaint" dead-ends at setState even
/// though `render → getRenderableElements → …` is fully call-connected after it.
/// Bridge it: for each class that has a `render` method, link every sibling method
/// whose body calls `this.setState(` → `render`. The setState gate keeps this to
/// React class components (a non-React class with a `render` method won't call
/// `this.setState`). Over-approximation (all setState methods reach render) is
/// accepted — it's reachability-correct, like the callback channels.
pub(super) fn react_render_edges(
    queries: &QueryBuilder,
    ctx: &dyn ResolutionContext,
) -> Result<Vec<Edge>> {
    let mut edges: Vec<Edge> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for cls in queries.get_nodes_by_kind(NodeKind::Class)? {
        let children = methods_of(queries, &cls.id)?;
        let Some(render) = children.iter().find(|n| n.name == "render") else {
            continue;
        };
        let mut added = 0usize;
        for m in &children {
            if added >= MAX_CALLBACKS_PER_CHANNEL {
                break;
            }
            if m.id == render.id {
                continue;
            }
            let Some(src) = node_source(ctx, m) else {
                continue;
            };
            if !SETSTATE_RE.is_match(&src) {
                continue;
            }
            let key = format!("{}>{}", m.id, render.id);
            if !seen.insert(key) {
                continue;
            }
            edges.push(synthesized_edge(
                &m.id,
                &render.id,
                Some(m.start_line),
                edge_meta(vec![
                    ("synthesizedBy", Value::from("react-render")),
                    ("via", Value::from("setState")),
                    (
                        "registeredAt",
                        Value::from(format!("{}:{}", render.file_path, render.start_line)),
                    ),
                ]),
            ));
            added += 1;
        }
    }
    Ok(edges)
}

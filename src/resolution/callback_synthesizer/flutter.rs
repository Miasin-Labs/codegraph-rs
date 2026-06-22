//! Flutter setState-to-build synthesis.

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
/// Flutter: `setState((){…})` / `this.setState`.
static FLUTTER_SETSTATE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u:\b)setState\s*\(").expect("valid regex"));

/// Phase 4b: Flutter setState → build (the Dart analog of react-render). In a
/// StatefulWidget's State class, `setState(() {…})` re-runs `build(context)`, but
/// that hop is framework-internal (Flutter calls build), so a flow like
/// "onPressed → _increment → setState → rebuilt UI" dead-ends at setState. Bridge
/// it: for each Dart class with a `build` method, link every sibling method whose
/// body calls `setState(` → `build`. The setState gate + `.dart` file keep this to
/// Flutter State classes. Over-approximation accepted (reachability-correct).
pub(super) fn flutter_build_edges(
    queries: &QueryBuilder,
    ctx: &dyn ResolutionContext,
) -> Result<Vec<Edge>> {
    let mut edges: Vec<Edge> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for cls in queries.get_nodes_by_kind(NodeKind::Class)? {
        let children = methods_of(queries, &cls.id)?;
        let Some(build) = children.iter().find(|n| n.name == "build") else {
            continue;
        };
        if !build.file_path.ends_with(".dart") {
            continue;
        }
        let mut added = 0usize;
        for m in &children {
            if added >= MAX_CALLBACKS_PER_CHANNEL {
                break;
            }
            if m.id == build.id {
                continue;
            }
            let Some(src) = node_source(ctx, m) else {
                continue;
            };
            if !FLUTTER_SETSTATE_RE.is_match(&src) {
                continue;
            }
            let key = format!("{}>{}", m.id, build.id);
            if !seen.insert(key) {
                continue;
            }
            edges.push(synthesized_edge(
                &m.id,
                &build.id,
                Some(m.start_line),
                edge_meta(vec![
                    ("synthesizedBy", Value::from("flutter-build")),
                    ("via", Value::from("setState")),
                    (
                        "registeredAt",
                        Value::from(format!("{}:{}", build.file_path, build.start_line)),
                    ),
                ]),
            ));
            added += 1;
        }
    }
    Ok(edges)
}

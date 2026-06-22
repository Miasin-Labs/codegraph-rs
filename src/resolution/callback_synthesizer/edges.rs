//! Shared synthesized-edge construction.

use serde_json::Value;

use crate::types::{Edge, EdgeKind, Metadata, Provenance};

/// Build edge metadata preserving the TS object-literal key order
/// (serde_json is compiled with `preserve_order`).
pub(super) fn edge_meta(entries: Vec<(&str, Value)>) -> Metadata {
    let mut m = Metadata::new();
    for (k, v) in entries {
        m.insert(k.to_string(), v);
    }
    m
}

pub(super) fn synthesized_edge(
    source: &str,
    target: &str,
    line: Option<u32>,
    metadata: Metadata,
) -> Edge {
    Edge {
        source: source.to_string(),
        target: target.to_string(),
        kind: EdgeKind::Calls,
        metadata: Some(metadata),
        line,
        column: None,
        provenance: Some(Provenance::Heuristic),
    }
}

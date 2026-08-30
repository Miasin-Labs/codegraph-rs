use super::fixture::extract_ts;
use crate::types::{Edge, EdgeKind, ExtractionResult, NodeKind};

pub(super) fn value_reference_edges(result: &ExtractionResult) -> Vec<&Edge> {
    result
        .edges
        .iter()
        .filter(|edge| {
            edge.kind == EdgeKind::References
                && edge
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("valueRef"))
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
        })
        .collect()
}

pub(super) fn value_reference_readers<'a>(
    result: &'a ExtractionResult,
    target_name: &str,
) -> Vec<&'a str> {
    let target_ids: Vec<_> = result
        .nodes
        .iter()
        .filter(|node| node.name == target_name)
        .map(|node| node.id.as_str())
        .collect();
    let mut readers: Vec<_> = value_reference_edges(result)
        .into_iter()
        .filter(|edge| target_ids.contains(&edge.target.as_str()))
        .filter_map(|edge| result.nodes.iter().find(|node| node.id == edge.source))
        .map(|node| node.name.as_str())
        .collect();
    readers.sort_unstable();
    readers.dedup();
    readers
}

#[test]
fn same_file_constant_readers_emit_three_impact_edges() {
    // Given: the audited source fixture with one constant and three readers.
    let source = [
        "export const TABLE_CONFIG = { rows: 10, cols: 4 };",
        "export function rowCount() { return TABLE_CONFIG.rows; }",
        "export function describeTable() { return `${TABLE_CONFIG.rows}x${TABLE_CONFIG.cols}`; }",
        "export const HEADER = TABLE_CONFIG.cols;",
    ]
    .join("\n");

    // When: the file is extracted through the generic tree-sitter wrapper.
    let result = extract_ts("config.ts", &source);

    // Then: four symbols and the source-equivalent three reader-to-value edges exist.
    assert_eq!(
        result
            .nodes
            .iter()
            .filter(|node| node.kind != NodeKind::File)
            .count(),
        4
    );
    let edges = value_reference_edges(&result);
    assert_eq!(edges.len(), 3);
    assert_eq!(
        value_reference_readers(&result, "TABLE_CONFIG"),
        ["HEADER", "describeTable", "rowCount"]
    );
}

#[test]
fn shadowed_constant_emits_no_value_reference_edges() {
    // Given: a file-level constant re-bound by a nested parameter and local declaration.
    let source = [
        "const Module = (function () {",
        "  return function (Module) {",
        "    var Module = typeof Module !== 'undefined' ? Module : {};",
        "    function locate() { return Module.path; }",
        "    return { locate };",
        "  };",
        "})();",
        "export default Module;",
    ]
    .join("\n");

    // When: the file is extracted.
    let result = extract_ts("bundled.ts", &source);

    // Then: the ambiguous outer binding has no reader edge.
    assert!(value_reference_edges(&result).is_empty());
}

#[test]
fn constant_reader_is_in_the_reference_impact_frontier() {
    // Given: the source impact-radius fixture.
    let source = [
        "export const COLOR_PALETTE = { red: '#f00', blue: '#00f' };",
        "export function pickRed() { return COLOR_PALETTE.red; }",
    ]
    .join("\n");

    // When: the file is extracted.
    let result = extract_ts("palette.ts", &source);

    // Then: reverse traversal from the constant has pickRed as its direct frontier.
    assert_eq!(
        value_reference_readers(&result, "COLOR_PALETTE"),
        ["pickRed"]
    );
}

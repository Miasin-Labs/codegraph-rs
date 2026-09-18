use crate::mcp::tools::schema::{ToolContent, ToolNotice, ToolResult, ToolResultMeta};

fn assert_matches_structured_content(projected: &ToolResult, expected: &serde_json::Value) {
    assert_eq!(projected.content.len(), 1);
    assert_eq!(projected.content[0].content_type, "text");
    let parsed: serde_json::Value = serde_json::from_str(projected.text()).unwrap();
    assert_eq!(&parsed, expected);
    assert_eq!(projected.structured_content.as_ref(), Some(expected));
}

#[test]
fn tool_result_text_preserves_original_human_text() {
    let result = ToolResult {
        content: vec![ToolContent {
            content_type: "text".into(),
            text: "human-readable result".into(),
        }],
        structured_content: Some(serde_json::json!({
            "schemaVersion": 1,
            "kind": "fixture",
        })),
        meta: None,
        is_error: None,
    };

    assert_eq!(result.text(), "human-readable result");
}

#[test]
fn mcp_projection_uses_existing_structured_content_as_canonical_json() {
    // Given
    let structured = serde_json::json!({
        "schemaVersion": 1,
        "kind": "search",
        "nodes": [{"name": "main", "score": 1.0}],
    });
    let original = ToolResult {
        content: vec![ToolContent {
            content_type: "text".into(),
            text: "human-readable search result".into(),
        }],
        structured_content: Some(structured.clone()),
        meta: None,
        is_error: None,
    };

    // When
    let projected = original.clone().into_mcp_projection().unwrap();

    // Then
    assert_matches_structured_content(&projected, &structured);
    assert_eq!(original.text(), "human-readable search result");
    println!("mcp_projection parsed fixture: {}", projected.text());
}

/// A tool without an output schema has no structured payload to mirror: its
/// own text goes out verbatim, not escaped inside a JSON envelope.
#[test]
fn mcp_projection_passes_text_only_results_through() {
    // Given
    let original = ToolResult {
        content: vec![ToolContent {
            content_type: "text".into(),
            text: "## Callers of run (1 found)\n\n- main (function) - src/main.rs:3".into(),
        }],
        structured_content: None,
        meta: None,
        is_error: None,
    };

    // When
    let projected = original.clone().into_mcp_projection().unwrap();

    // Then
    assert_eq!(projected.content.len(), 1);
    assert_eq!(projected.text(), original.text());
    assert!(projected.structured_content.is_none());
    assert_eq!(projected.is_error, None);
}

/// Text-only output is bounded by the default MCP output budget even when
/// `CODEGRAPH_MAX_OUTPUT_CHARS` is unset, and says it was cut.
#[test]
fn mcp_projection_bounds_text_only_results_by_default() {
    if std::env::var_os("CODEGRAPH_MAX_OUTPUT_CHARS").is_some() {
        return;
    }
    let line = format!("- {} (function) - src/lib.rs:1\n", "x".repeat(60));
    let original = ToolResult {
        content: vec![ToolContent {
            content_type: "text".into(),
            text: line.repeat(2_000),
        }],
        structured_content: None,
        meta: None,
        is_error: None,
    };

    let projected = original.into_mcp_projection().unwrap();

    let budget = crate::mcp::tools::format::mcp_output_budget();
    assert!(
        projected.text().len() <= budget,
        "{} > {budget}",
        projected.text().len()
    );
    assert!(projected.text().ends_with("... (output truncated)"));
    // Cut on a line boundary, so no row is half-sent.
    let body = projected
        .text()
        .trim_end_matches("\n\n... (output truncated)");
    assert!(body.lines().all(|row| row.ends_with("src/lib.rs:1")));
}

#[test]
fn mcp_projection_preserves_structured_error_and_is_error() {
    // Given
    let structured = serde_json::json!({
        "schemaVersion": 1,
        "kind": "error",
        "error": {"code": "tool_error", "message": "index unavailable"},
    });
    let original = ToolResult {
        content: vec![ToolContent {
            content_type: "text".into(),
            text: "index unavailable".into(),
        }],
        structured_content: Some(structured.clone()),
        meta: None,
        is_error: Some(true),
    };

    // When
    let projected = original.clone().into_mcp_projection().unwrap();

    // Then
    assert_matches_structured_content(&projected, &structured);
    assert_eq!(projected.is_error, Some(true));
    assert_eq!(original.text(), "index unavailable");
}

#[test]
fn mcp_projection_preserves_meta_notices() {
    // Given
    let structured = serde_json::json!({"schemaVersion": 1, "kind": "status"});
    let original = ToolResult {
        content: vec![ToolContent {
            content_type: "text".into(),
            text: "status report".into(),
        }],
        structured_content: Some(structured.clone()),
        meta: Some(ToolResultMeta {
            notices: vec![ToolNotice {
                kind: "stale_index".into(),
                severity: "warning".into(),
                message: "Index has pending files".into(),
                files: Vec::new(),
                data: Some(serde_json::json!({"pending": 2})),
            }],
        }),
        is_error: None,
    };
    let expected_meta = serde_json::to_value(&original.meta).unwrap();

    // When
    let projected = original.clone().into_mcp_projection().unwrap();

    // Then
    assert_matches_structured_content(&projected, &structured);
    assert_eq!(
        serde_json::to_value(&projected.meta).unwrap(),
        expected_meta
    );
    assert_eq!(original.text(), "status report");
}

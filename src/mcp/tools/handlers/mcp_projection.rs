use crate::mcp::tools::schema::{
    NoticeKind,
    ToolContent,
    ToolNotice,
    ToolNoticeFile,
    ToolResult,
    ToolResultMeta,
};

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

fn notice(kind: NoticeKind, message: &str, files: &[&str]) -> ToolNotice {
    ToolNotice {
        kind,
        severity: "warning".into(),
        message: message.into(),
        files: files
            .iter()
            .map(|path| ToolNoticeFile {
                path: (*path).to_string(),
                age_ms: 5,
                status: "pending sync".into(),
            })
            .collect(),
        data: Some(serde_json::json!({"pending": files.len()})),
    }
}

fn with_notices(result: ToolResult, notices: Vec<ToolNotice>) -> ToolResult {
    ToolResult {
        meta: Some(ToolResultMeta { notices }),
        ..result
    }
}

fn structured(payload: serde_json::Value) -> ToolResult {
    ToolResult {
        content: vec![ToolContent {
            content_type: "text".into(),
            text: "human-readable report".into(),
        }],
        structured_content: Some(payload),
        meta: None,
        is_error: None,
    }
}

fn text_only(text: &str) -> ToolResult {
    ToolResult {
        content: vec![ToolContent {
            content_type: "text".into(),
            text: text.into(),
        }],
        structured_content: None,
        meta: None,
        is_error: None,
    }
}

#[test]
fn mcp_projection_preserves_meta_notices() {
    // Given
    let original = with_notices(
        structured(serde_json::json!({"schemaVersion": 1, "kind": "status"})),
        vec![notice(
            NoticeKind::StaleIndex,
            "Index has pending files",
            &["src/a.ts"],
        )],
    );
    let expected_meta = serde_json::to_value(&original.meta).unwrap();

    // When
    let projected = original.clone().into_mcp_projection().unwrap();

    // Then: `_meta` keeps every notice in full detail, and the payload the
    // model reads carries it too.
    assert_eq!(
        serde_json::to_value(&projected.meta).unwrap(),
        expected_meta
    );
    assert_matches_structured_content(
        &projected,
        &serde_json::json!({
            "schemaVersion": 1,
            "kind": "status",
            "notices": [{
                "kind": "stale_index",
                "message": "Index has pending files",
                "files": ["src/a.ts"],
            }],
        }),
    );
    assert_eq!(original.text(), "human-readable report");
}

/// Hosts show the model `content` text or `structuredContent`, rarely
/// `_meta`, so a structured payload carries its notices itself: in front of
/// the results, one entry per kind, whole-index kinds first.
#[test]
fn mcp_projection_puts_notices_in_the_structured_payload_before_the_results() {
    let original = with_notices(
        structured(serde_json::json!({
            "schemaVersion": 2,
            "kind": "node",
            "matchCount": 1,
            "matches": [{"name": "run", "file": "src/a.ts"}],
        })),
        vec![
            notice(NoticeKind::StaleIndex, "stale", &["src/a.ts"]),
            notice(NoticeKind::AutoSyncDisabled, "frozen", &[]),
            notice(
                NoticeKind::StaleIndex,
                "stale again",
                &["src/b.ts", "src/a.ts"],
            ),
        ],
    );

    let projected = original.into_mcp_projection().unwrap();

    let payload = projected.structured_content.clone().unwrap();
    let keys: Vec<&str> = payload
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        ["schemaVersion", "kind", "notices", "matchCount", "matches"]
    );
    assert_eq!(
        payload["notices"],
        serde_json::json!([
            {"kind": "auto_sync_disabled", "message": "frozen"},
            {"kind": "stale_index", "message": "stale", "files": ["src/a.ts", "src/b.ts"]},
        ])
    );
    assert_matches_structured_content(&projected, &payload);
}

/// However many files are pending (a branch switch can leave thousands),
/// the notice lists a few and counts the rest.
#[test]
fn mcp_projection_bounds_the_files_a_notice_lists() {
    let paths: Vec<String> = (0..25).map(|i| format!("src/file{i}.ts")).collect();
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let original = with_notices(
        structured(serde_json::json!({"schemaVersion": 2, "kind": "search", "results": []})),
        vec![notice(NoticeKind::StaleIndex, "stale", &refs)],
    );

    let projected = original.into_mcp_projection().unwrap();

    let entry = &projected.structured_content.unwrap()["notices"][0];
    assert_eq!(entry["files"].as_array().unwrap().len(), 10);
    assert_eq!(entry["files"][0], "src/file0.ts");
    assert_eq!(entry["filesOmitted"], 15);
}

/// A text result has no payload to carry notices, so it leads with them.
#[test]
fn mcp_projection_leads_a_text_result_with_its_notices() {
    let original = with_notices(
        text_only("## Callers of run (1 found)\n\n- main (function) - src/main.rs:3"),
        vec![
            notice(
                NoticeKind::StaleIndex,
                "Changed since the sync.",
                &["src/main.rs"],
            ),
            notice(NoticeKind::AutoSyncDisabled, "Auto-sync is off.", &[]),
        ],
    );

    let projected = original.into_mcp_projection().unwrap();

    assert!(projected.structured_content.is_none());
    assert_eq!(
        projected.text(),
        "⚠️ Auto-sync is off.\n⚠️ Changed since the sync. Files: src/main.rs\n\n\
         ## Callers of run (1 found)\n\n- main (function) - src/main.rs:3"
    );
    assert_eq!(projected.meta.unwrap().notices.len(), 2);
}

#[test]
fn mcp_projection_counts_omitted_files_in_a_text_banner() {
    let paths: Vec<String> = (0..12).map(|i| format!("f{i}.rs")).collect();
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let original = with_notices(
        text_only("body"),
        vec![notice(NoticeKind::StaleIndex, "Stale.", &refs)],
    );

    let projected = original.into_mcp_projection().unwrap();

    let banner = projected.text().lines().next().unwrap();
    assert!(
        banner.starts_with("⚠️ Stale. Files: f0.rs, f1.rs,"),
        "{banner}"
    );
    assert!(banner.ends_with("f9.rs (+2 more)"), "{banner}");
}

/// An error is not a result to distrust; it carries no notices.
#[test]
fn mcp_projection_never_attaches_notices_to_an_error() {
    let original = with_notices(
        ToolResult {
            is_error: Some(true),
            ..structured(serde_json::json!({
                "schemaVersion": 1,
                "kind": "error",
                "error": {"code": "tool_error", "message": "boom"},
            }))
        },
        vec![notice(NoticeKind::AutoSyncDisabled, "frozen", &[])],
    );

    let projected = original.into_mcp_projection().unwrap();

    assert!(
        projected
            .structured_content
            .unwrap()
            .get("notices")
            .is_none()
    );
}

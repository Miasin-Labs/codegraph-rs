use serde_json::json;

use super::*;
use crate::mcp::tools::{ToolContent, ToolResult};

fn result(path: &str, start: usize, end: usize, source: &str) -> ToolResult {
    let payload = json!({
        "kind": "explore",
        "sourceFiles": [{
            "path": path,
            "chunks": [{ "startLine": start, "endLine": end, "source": source }]
        }]
    });
    ToolResult {
        content: vec![ToolContent {
            content_type: "text".into(),
            text: String::new(),
        }],
        structured_content: Some(payload),
        meta: None,
        is_error: None,
    }
}

#[test]
fn counts_past_the_retained_call_bound() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.rs"), "fn a() {}\n").unwrap();
    let mut state = ExploreSessionState::default();
    for _ in 0..MAX_CALLS + 5 {
        state.record(root.path(), &result("a.rs", 1, 1, "fn a() {}"));
    }
    let project = state.view_for(root.path());
    assert_eq!(project.call_count, (MAX_CALLS + 5) as u64);
    assert_eq!(project.calls.len(), MAX_VIEW_CALLS);
    assert_eq!(project.calls.last().unwrap().index, (MAX_CALLS + 5) as u64);
}

#[test]
fn evicts_the_least_recently_used_project() {
    let parent = tempfile::tempdir().unwrap();
    let mut roots = Vec::new();
    let mut state = ExploreSessionState::default();
    for index in 0..MAX_PROJECTS + 2 {
        let root = parent.path().join(index.to_string());
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.rs"), "fn a() {}\n").unwrap();
        state.record(&root, &result("a.rs", 1, 1, "fn a() {}"));
        roots.push(root);
    }
    assert_eq!(state.view_for(&roots[0]).call_count, 0);
    assert_eq!(state.view_for(roots.last().unwrap()).call_count, 1);
}

#[test]
fn fingerprints_gate_cross_call_ranges() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.rs"), "one\ntwo\nthree\n").unwrap();
    let mut state = ExploreSessionState::default();
    state.record(root.path(), &result("a.rs", 1, 3, "one\ntwo\nthree"));
    let prior = state.view_for(root.path());
    let fingerprint = file_fingerprint(root.path(), "a.rs").unwrap();
    println!("original_fingerprint={fingerprint}");
    assert_eq!(
        served_ranges(&prior, "a.rs", &fingerprint),
        [LineRange { start: 1, end: 3 }]
    );
    std::fs::write(root.path().join("a.rs"), "one\nchanged\nthree\n").unwrap();
    let edited = file_fingerprint(root.path(), "a.rs").unwrap();
    println!("edited_fingerprint={edited}");
    assert!(served_ranges(&prior, "a.rs", &edited).is_empty());
}

#[test]
fn bounds_files_and_coalesces_ranges() {
    let root = tempfile::tempdir().unwrap();
    let source_files = (0..MAX_FILES + 4)
        .map(|index| {
            json!({
                "path": format!("f{index}.rs"),
                "chunks": [
                    { "startLine": 1, "endLine": 5, "source": "a\nb\nc\nd\ne" },
                    { "startLine": 6, "endLine": 12, "source": "f\ng\nh\ni\nj\nk\nlarger" }
                ]
            })
        })
        .collect::<Vec<_>>();
    for index in 0..MAX_FILES + 4 {
        std::fs::write(root.path().join(format!("f{index}.rs")), "source\n").unwrap();
    }
    let mut state = ExploreSessionState::default();
    let mut value = result("unused.rs", 1, 1, "x");
    value.structured_content = Some(json!({ "kind": "explore", "sourceFiles": source_files }));
    state.record(root.path(), &value);
    let files = &state.view_for(root.path()).calls[0].files;
    assert_eq!(files.len(), MAX_FILES);
    assert_eq!(files[0].ranges, [LineRange { start: 1, end: 12 }]);
}

#[test]
fn separate_connection_states_never_share_history() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.rs"), "fn a() {}\n").unwrap();
    let mut first = ExploreSessionState::default();
    let second = ExploreSessionState::default();
    first.record(root.path(), &result("a.rs", 1, 1, "fn a() {}"));
    assert_eq!(first.view_for(root.path()).call_count, 1);
    assert_eq!(second.view_for(root.path()).call_count, 0);
}

#[test]
fn caps_ranges_by_retaining_the_largest_spans() {
    let ranges = (0..MAX_RANGES + 5)
        .map(|index| LineRange {
            start: index * 100 + 1,
            end: index * 100 + index + 2,
        })
        .collect();
    let (ranges, truncated) = coalesce(ranges);
    assert!(truncated);
    assert_eq!(ranges.len(), MAX_RANGES);
    assert!(
        ranges
            .iter()
            .any(|range| range.start == (MAX_RANGES + 4) * 100 + 1)
    );
}

fn tool_result(payload: serde_json::Value) -> ToolResult {
    ToolResult {
        content: vec![ToolContent {
            content_type: "text".into(),
            text: String::new(),
        }],
        structured_content: Some(payload),
        meta: None,
        is_error: None,
    }
}

#[test]
fn records_node_file_views_so_other_tools_can_dedup() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.rs"), "fn a() {}\nfn b() {}\n").unwrap();
    let mut state = ExploreSessionState::default();
    let payload = json!({
        "kind": "file",
        "path": "a.rs",
        "startLine": 1,
        "endLine": 2,
        "source": "fn a() {}\nfn b() {}",
    });
    state.record(root.path(), &tool_result(payload));
    let view = state.view_for(root.path());
    let fingerprint = file_fingerprint(root.path(), "a.rs").expect("fingerprint");
    let served = served_ranges(&view, "a.rs", &fingerprint);
    assert_eq!(served.len(), 1);
    assert_eq!((served[0].start, served[0].end), (1, 2));
    assert!(range_already_sent(&view, root.path(), "a.rs", 1, 2));
    assert!(!range_already_sent(&view, root.path(), "a.rs", 1, 3));
}

#[test]
fn records_node_symbol_code_at_its_lines() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("a.rs"),
        "use x;\nfn a() {\n    1\n}\nfn b() {}\n",
    )
    .unwrap();
    let mut state = ExploreSessionState::default();
    let payload = json!({
        "kind": "node",
        "matchCount": 2,
        "matches": [
            { "name": "a", "kind": "function", "file": "a.rs", "line": 2, "endLine": 4,
              "code": "fn a() {\n    1\n}" },
            { "name": "b", "kind": "function", "file": "a.rs", "line": 5, "endLine": 5 }
        ],
    });
    state.record(root.path(), &tool_result(payload));
    let view = state.view_for(root.path());
    assert!(range_already_sent(&view, root.path(), "a.rs", 2, 4));
    assert!(!range_already_sent(&view, root.path(), "a.rs", 5, 5));
}

/// A node call that carried no source must not evict one that did from the
/// bounded view.
#[test]
fn node_calls_without_source_are_not_recorded() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.rs"), "fn a() {}\n").unwrap();
    let mut state = ExploreSessionState::default();
    state.record(
        root.path(),
        &tool_result(json!({ "kind": "file", "path": "a.rs", "symbolCount": 1 })),
    );
    assert_eq!(state.view_for(root.path()).call_count, 0);
}

/// Source some later pass shortened no longer spans the lines it claims, so
/// it must not be recorded as delivered.
#[test]
fn shortened_source_is_not_recorded_as_sent() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.rs"), "one\ntwo\nthree\n").unwrap();
    let mut state = ExploreSessionState::default();
    state.record(
        root.path(),
        &tool_result(json!({
            "kind": "file",
            "path": "a.rs",
            "startLine": 1,
            "endLine": 3,
            "source": "one... [truncated]",
        })),
    );
    let view = state.view_for(root.path());
    assert!(!range_already_sent(&view, root.path(), "a.rs", 1, 1));
}

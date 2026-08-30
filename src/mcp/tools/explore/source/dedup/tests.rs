use super::*;
use crate::mcp::explore_session::{CallRecord, FileEmission};
use crate::mcp::tools::explore::types::{SourceChunk, SourceChunkMode};

fn rendered(root: &std::path::Path, lines: usize) -> (RenderedFile, ProjectState) {
    let source = (1..=lines)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(root.join("a.rs"), &source).unwrap();
    let fingerprint = file_fingerprint(root, "a.rs");
    let chunk = SourceChunk {
        start_line: 1,
        end_line: lines,
        mode: SourceChunkMode::Whole,
        symbols: vec!["answer".into()],
        source: source.clone(),
        unicode_hazards: Vec::new(),
    };
    let prior = ProjectState {
        project_root: root.to_string_lossy().into_owned(),
        call_count: 1,
        response_bytes: source.len() as u64,
        calls: vec![CallRecord {
            index: 1,
            response_bytes: source.len() as u64,
            files: vec![FileEmission {
                path: "a.rs".into(),
                ranges: vec![LineRange {
                    start: 1,
                    end: lines,
                }],
                bytes: source.len(),
                fingerprint,
            }],
        }],
    };
    (
        RenderedFile {
            header: "a.rs".into(),
            language: "rust".into(),
            body: source,
            chunks: vec![chunk],
            cost: 500,
        },
        prior,
    )
}

#[test]
fn withholds_a_proven_covered_chunk_of_eight_lines() {
    let root = tempfile::tempdir().unwrap();
    let (rendered, prior) = rendered(root.path(), 8);
    let result = apply(
        rendered,
        DedupRequest {
            root: root.path(),
            path: "a.rs",
            prior: Some(&prior),
            line_numbers: true,
        },
    );
    assert!(result.rendered.is_none());
    assert_eq!(result.covered, [LineRange { start: 1, end: 8 }]);
}

#[test]
fn repeats_a_short_overlap_instead_of_shredding_the_block() {
    let root = tempfile::tempdir().unwrap();
    let (rendered, prior) = rendered(root.path(), 7);
    let result = apply(
        rendered,
        DedupRequest {
            root: root.path(),
            path: "a.rs",
            prior: Some(&prior),
            line_numbers: true,
        },
    );
    assert!(result.rendered.is_some());
    assert!(result.covered.is_empty());
}

#[test]
fn reemits_after_the_file_fingerprint_changes() {
    let root = tempfile::tempdir().unwrap();
    let (rendered, prior) = rendered(root.path(), 8);
    std::fs::write(root.path().join("a.rs"), "changed\nbytes\n").unwrap();
    let result = apply(
        rendered,
        DedupRequest {
            root: root.path(),
            path: "a.rs",
            prior: Some(&prior),
            line_numbers: true,
        },
    );
    assert!(result.rendered.is_some());
    assert!(result.covered.is_empty());
}

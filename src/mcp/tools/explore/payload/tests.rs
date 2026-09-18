use super::super::types::{SourceChunk, SourceChunkMode};
use super::*;

#[test]
fn adaptive_cap_applies_to_the_complete_serialized_payload() {
    let literals = LiteralContentMatches::default();
    let relationships = (0..400)
        .map(|index| ExploreRelationship {
            kind: "calls".to_string(),
            source: format!("source_{index}_{}", "x".repeat(40)),
            target: format!("target_{index}_{}", "y".repeat(40)),
        })
        .collect();
    let additional_files = (0..20)
        .map(|file| ExploreAdditionalFile {
            path: format!("src/file_{file}.rs"),
            symbols: (0..100)
                .map(|symbol| format!("symbol_{file}_{symbol}_{}", "z".repeat(20)))
                .collect(),
        })
        .collect();

    let payload = explore_payload(ExplorePayloadInput {
        query: "broad metadata",
        total_symbols: 400,
        total_files: 20,
        files_included: 0,
        source_files: Vec::new(),
        back_references: Vec::new(),
        relationships,
        additional_files,
        related_files: Vec::new(),
        related_windows: Vec::new(),
        external: Vec::new(),
        literal_matches: &literals,
        trimmed: false,
        omissions: Vec::new(),
        max_output_chars: 16_000,
    })
    .unwrap();
    let serialized = serde_json::to_string(&payload).unwrap();

    assert!(serialized.len() <= 16_000, "{}", serialized.len());
    assert_eq!(payload["trimmed"], true);
}

fn source_file(path: &str, lines: usize) -> StructuredSourceFile {
    let text = (1..=lines)
        .map(|line| format!("line {line} of {path} {}", "w".repeat(40)))
        .collect::<Vec<_>>();
    let refs = text.iter().map(String::as_str).collect::<Vec<_>>();
    StructuredSourceFile {
        path: path.to_string(),
        language: "rust".to_string(),
        chunks: vec![
            SourceChunk::from_lines(&refs, 1, lines as i64, SourceChunkMode::Excerpt, Vec::new())
                .unwrap(),
        ],
        source_truncated: false,
    }
}

fn related_row(index: usize) -> ExploreRelatedFile {
    ExploreRelatedFile {
        path: format!("src/related/file_{index}.rs"),
        reason: format!("calls seed_symbol_{index}"),
        symbol: Some(format!("caller_{index}")),
        line: Some(index + 1),
    }
}

fn payload_input(
    literals: &LiteralContentMatches,
    source_files: Vec<StructuredSourceFile>,
    related_files: Vec<ExploreRelatedFile>,
    related_windows: Vec<StructuredSourceFile>,
    max_output_chars: usize,
) -> ExplorePayloadInput<'_> {
    ExplorePayloadInput {
        query: "q",
        total_symbols: 1,
        total_files: source_files.len(),
        files_included: source_files.len(),
        source_files,
        back_references: Vec::new(),
        relationships: Vec::new(),
        additional_files: Vec::new(),
        related_files,
        related_windows,
        external: Vec::new(),
        literal_matches: literals,
        trimmed: false,
        omissions: Vec::new(),
        max_output_chars,
    }
}

#[test]
fn related_windows_are_added_only_while_they_fit() {
    let literals = LiteralContentMatches::default();
    let windows = vec![
        source_file("src/big_window.rs", 60),
        source_file("src/small_window.rs", 2),
    ];
    let payload = explore_payload(payload_input(
        &literals,
        vec![source_file("src/main.rs", 10)],
        vec![related_row(0)],
        windows,
        3_000,
    ))
    .unwrap();
    let paths: Vec<&str> = payload["sourceFiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        ["src/main.rs", "src/small_window.rs"],
        "a window that does not fit is skipped"
    );
    assert_eq!(
        payload["trimmed"], false,
        "skipping an optional window is not a trim"
    );
    assert!(serde_json::to_string(&payload).unwrap().len() <= 3_000);
}

#[test]
fn source_overshoot_is_shed_from_the_lowest_ranked_file_before_related_rows() {
    let literals = LiteralContentMatches::default();
    let sources = vec![source_file("src/top.rs", 20), source_file("src/low.rs", 20)];
    let rows = (0..10).map(related_row).collect::<Vec<_>>();
    let full = explore_payload(payload_input(
        &literals,
        sources.clone(),
        rows.clone(),
        Vec::new(),
        100_000,
    ))
    .unwrap();
    let full_len = serde_json::to_string(&full).unwrap().len();

    let payload = explore_payload(payload_input(
        &literals,
        sources,
        rows,
        Vec::new(),
        full_len - 100,
    ))
    .unwrap();
    let files = payload["sourceFiles"].as_array().unwrap();
    assert_eq!(
        files[0]["sourceTruncated"], false,
        "the top-ranked file keeps its source: {payload}"
    );
    assert_eq!(files[1]["sourceTruncated"], true, "{payload}");
    assert_eq!(
        payload["relatedFiles"].as_array().unwrap().len(),
        10,
        "rows had room reserved"
    );
}

fn external_row(index: usize) -> super::super::external::ExploreExternal {
    super::super::external::ExploreExternal {
        graph: "serde_json@1.0.150".to_string(),
        symbol: format!("Deserializer::method_{index}"),
        kind: "method",
        file: "src/de.rs".to_string(),
        line: Some(index as u32 + 10),
        signature: Some(format!("(&mut self, v: {}) -> Result<()>", "T".repeat(30))),
        from: "load".to_string(),
        calls: 2,
        unavailable: None,
    }
}

#[test]
fn external_rows_are_shed_before_related_rows_and_the_payload_fits() {
    // No source to shed first: the rows are what must give.
    let literals = LiteralContentMatches::default();
    let rows = (0..4).map(related_row).collect::<Vec<_>>();
    let mut input = payload_input(&literals, Vec::new(), rows.clone(), Vec::new(), 100_000);
    input.external = (0..8).map(external_row).collect();
    let full = explore_payload(input).unwrap();
    assert_eq!(full["external"].as_array().unwrap().len(), 8);
    let full_len = serde_json::to_string(&full).unwrap().len();

    let mut input = payload_input(&literals, Vec::new(), rows, Vec::new(), full_len - 150);
    input.external = (0..8).map(external_row).collect();
    let payload = explore_payload(input).unwrap();
    assert!(serde_json::to_string(&payload).unwrap().len() <= full_len - 150);
    let external = payload["external"].as_array().unwrap().len();
    assert!(external < 8, "{payload}");
    assert_eq!(payload["relatedFiles"].as_array().unwrap().len(), 4);
    assert_eq!(payload["trimmed"], true);
}

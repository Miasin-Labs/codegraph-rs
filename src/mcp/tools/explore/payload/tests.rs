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

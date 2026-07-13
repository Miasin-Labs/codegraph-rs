fn hazard_entries(chunk: &serde_json::Value) -> Vec<(u64, String, u64, u64)> {
    chunk["unicodeHazards"]
        .as_array()
        .expect("unicodeHazards array")
        .iter()
        .map(|h| {
            (
                h["codepoint"].as_u64().unwrap(),
                h["category"].as_str().unwrap().to_string(),
                h["line"].as_u64().unwrap(),
                h["column"].as_u64().unwrap(),
            )
        })
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn explore_reports_unicode_hazards_without_normalizing_source() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    // Hazards live in a trailing comment so parsing still yields the function.
    let hazard_line = "// hazard \u{202E}\u{200B}\u{E000}\u{FDD0}\u{0007} end";
    let source = format!("export function target(): number {{\n  return 1;\n}}\n{hazard_line}\n");
    write(&dir.path().join("src/state.ts"), &source);
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let result = handler.execute("codegraph_explore", &json!({ "query": "target" }));
    assert_ne!(
        result.is_error,
        Some(true),
        "explore errored: {}",
        result.text()
    );
    let structured = result.structured_content.as_ref().expect("structured explore");
    let file = structured["sourceFiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == "src/state.ts")
        .expect("state.ts source file");
    let chunk = &file["chunks"][0];

    // Source is preserved byte-for-byte; hazards are reported, not stripped.
    let raw = chunk["source"].as_str().unwrap();
    for hazard in ['\u{202E}', '\u{200B}', '\u{E000}', '\u{FDD0}', '\u{0007}'] {
        assert!(raw.contains(hazard), "source dropped {:#06X}", hazard as u32);
    }

    let hazards = hazard_entries(chunk);
    let expected = [
        (0x202E, "bidi_control", 11),
        (0x200B, "zero_width", 12),
        (0xE000, "private_use", 13),
        (0xFDD0, "noncharacter", 14),
        (0x0007, "control_char", 15),
    ];
    for (codepoint, category, column) in expected {
        let found = hazards
            .iter()
            .find(|(cp, _, _, _)| *cp == codepoint)
            .unwrap_or_else(|| panic!("missing hazard {codepoint:#06X}: {hazards:?}"));
        assert_eq!(found.1, category, "wrong category for {codepoint:#06X}");
        assert_eq!(found.2, 4, "wrong line for {codepoint:#06X}");
        assert_eq!(found.3, column as u64, "wrong column for {codepoint:#06X}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn explore_unicode_hazards_ignore_benign_non_ascii() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let source =
        "// café 日本語 résumé — accented and CJK are fine\nexport function target(): number {\n  return 1;\n}\n";
    write(&dir.path().join("src/state.ts"), source);
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let result = handler.execute("codegraph_explore", &json!({ "query": "target" }));
    let structured = result.structured_content.as_ref().expect("structured explore");
    let files = structured["sourceFiles"].as_array().unwrap();
    assert!(!files.is_empty(), "expected the source file to render");
    for file in files {
        for chunk in file["chunks"].as_array().unwrap() {
            let hazards = chunk["unicodeHazards"].as_array().unwrap();
            assert!(
                hazards.is_empty(),
                "benign non-ASCII flagged as hazard: {hazards:?}"
            );
            // The benign text is still present verbatim.
            let raw = chunk["source"].as_str().unwrap();
            assert!(raw.contains("café") || raw.contains("日本語") || raw.contains("target"));
        }
    }
}

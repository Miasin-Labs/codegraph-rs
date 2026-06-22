use crate::support::*;

// ===========================================================================
// Installer Config Writer (port of __tests__/installer.test.ts)
// ===========================================================================

#[test]
fn config_writer_creates_mcp_json_when_missing() {
    let env = TestEnv::new();
    // write_mcp_config reads .mcp.json - if it doesn't exist, it should create it
    write_mcp_config(Location::Local);

    let mcp_json = env.cwd().join(".mcp.json");
    assert!(mcp_json.exists());

    let content = read_json(&mcp_json);
    assert!(content["mcpServers"].is_object());
    assert!(content["mcpServers"]["codegraph"].is_object());
}

#[test]
fn config_writer_handles_corrupted_json_by_creating_backup() {
    let env = TestEnv::new();
    // Create a corrupted .mcp.json
    let mcp_json = env.cwd().join(".mcp.json");
    fs::write(&mcp_json, "{ this is not valid json !!!").unwrap();

    // Should not panic - gracefully handles corruption.
    // (TS asserts a console.warn spy fired; the Rust port prints the same
    // two warning lines to stderr, which we can't intercept here.)
    write_mcp_config(Location::Local);

    // Backup should exist
    let backup = PathBuf::from(format!("{}.backup", mcp_json.display()));
    assert!(backup.exists());
    // Original backup content should be the corrupted content
    assert!(read(&backup).contains("this is not valid json"));

    // New file should be valid JSON with codegraph config
    let content = read_json(&mcp_json);
    assert!(content["mcpServers"]["codegraph"].is_object());
}

#[test]
fn config_writer_preserves_existing_valid_config() {
    let env = TestEnv::new();
    let mcp_json = env.cwd().join(".mcp.json");
    write(
        &mcp_json,
        &serde_json::to_string_pretty(&json!({
            "mcpServers": { "other": { "command": "other-tool" } },
            "customField": "preserved",
        }))
        .unwrap(),
    );

    write_mcp_config(Location::Local);

    let content = read_json(&mcp_json);
    assert!(content["mcpServers"]["codegraph"].is_object());
    assert!(content["mcpServers"]["other"].is_object());
    assert_eq!(content["customField"], "preserved");
}

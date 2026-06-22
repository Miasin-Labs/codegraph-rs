use crate::support::*;

#[test]
fn gemini_install_writes_settings_json_and_no_gemini_md() {
    let env = TestEnv::new();
    let gemini = get_target("gemini").unwrap();
    let result = gemini.install(Location::Global, &auto_allow());
    let settings = env.home().join(".gemini").join("settings.json");
    let gemini_md = env.home().join(".gemini").join("GEMINI.md");
    assert!(result.files.iter().any(|f| f.path == settings));
    assert!(!result.files.iter().any(|f| f.path == gemini_md));
    assert!(!gemini_md.exists());

    let cfg = read_json(&settings);
    assert_eq!(
        cfg["mcpServers"]["codegraph"],
        json!({ "type": "stdio", "command": "codegraph", "args": ["serve", "--mcp"] })
    );
}

#[test]
fn gemini_install_preserves_preexisting_settings() {
    let env = TestEnv::new();
    let gemini = get_target("gemini").unwrap();
    let settings = env.home().join(".gemini").join("settings.json");
    write(
        &settings,
        &pretty(&json!({ "security": { "auth": { "selectedType": "oauth-personal" } } })),
    );

    gemini.install(Location::Global, &auto_allow());

    let after = read_json(&settings);
    assert_eq!(after["security"]["auth"]["selectedType"], "oauth-personal");
    assert!(after["mcpServers"]["codegraph"].is_object());
}

#[test]
fn gemini_uninstall_strips_codegraph_but_leaves_preexisting_settings() {
    let env = TestEnv::new();
    let gemini = get_target("gemini").unwrap();
    let settings = env.home().join(".gemini").join("settings.json");
    write(
        &settings,
        &pretty(&json!({ "security": { "auth": { "selectedType": "oauth-personal" } } })),
    );

    gemini.install(Location::Global, &auto_allow());
    gemini.uninstall(Location::Global);

    let after = read_json(&settings);
    assert_eq!(after["security"]["auth"]["selectedType"], "oauth-personal");
    assert!(after.get("mcpServers").is_none());
}

#[test]
fn gemini_local_install_writes_local_settings_and_never_gemini_md() {
    let env = TestEnv::new();
    let gemini = get_target("gemini").unwrap();
    let result = gemini.install(Location::Local, &auto_allow());
    let paths: Vec<String> = result
        .files
        .iter()
        .map(|f| f.path.to_string_lossy().replace('\\', "/"))
        .collect();
    assert!(paths.iter().any(|p| p.ends_with("/.gemini/settings.json")));
    assert!(!paths.iter().any(|p| p.ends_with("/GEMINI.md")));
    assert!(!env.cwd().join("GEMINI.md").exists());
}

#[test]
fn gemini_uninstall_strips_leftover_gemini_md_block_keeping_user_content() {
    let env = TestEnv::new();
    let gemini = get_target("gemini").unwrap();
    let gemini_md = env.home().join(".gemini").join("GEMINI.md");
    write(
        &gemini_md,
        &format!("# My personal Gemini context\n\nAlways respond concisely.\n\n{LEGACY_BLOCK}\n"),
    );

    gemini.uninstall(Location::Global);

    let body = read(&gemini_md);
    assert!(body.contains("# My personal Gemini context"));
    assert!(body.contains("Always respond concisely."));
    assert!(!body.contains("CODEGRAPH_START"));
}

#[test]
fn gemini_and_antigravity_coexist() {
    let env = TestEnv::new();
    let gemini = get_target("gemini").unwrap();
    let antigravity = get_target("antigravity").unwrap();
    gemini.install(Location::Global, &auto_allow());
    antigravity.install(Location::Global, &auto_allow());

    let cli_cfg = read_json(&env.home().join(".gemini").join("settings.json"));
    // Antigravity lands on the LEGACY path here since no .migrated marker
    // was planted — same end-to-end check either way.
    let ide_cfg = read_json(
        &env.home()
            .join(".gemini")
            .join("antigravity")
            .join("mcp_config.json"),
    );
    assert!(cli_cfg["mcpServers"]["codegraph"].is_object());
    assert!(ide_cfg["mcpServers"]["codegraph"].is_object());

    // Uninstall one — the other's MCP entry must survive.
    antigravity.uninstall(Location::Global);
    let cli_after = read_json(&env.home().join(".gemini").join("settings.json"));
    assert!(cli_after["mcpServers"]["codegraph"].is_object());
}

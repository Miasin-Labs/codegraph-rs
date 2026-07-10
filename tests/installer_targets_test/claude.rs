use crate::support::*;

#[test]
fn claude_local_install_writes_mcp_json_not_claude_json() {
    let env = TestEnv::new();
    let claude = get_target("claude").unwrap();
    let result = claude.install(Location::Local, &no_allow());
    // The MCP entry lands in ./.mcp.json — the file Claude Code reads.
    assert!(result.files.iter().any(|f| {
        f.path
            .to_string_lossy()
            .replace('\\', "/")
            .ends_with("/.mcp.json")
    }));
    assert!(env.cwd().join(".mcp.json").exists());
    assert!(!env.cwd().join(".claude.json").exists());
    let cfg = read_json(&env.cwd().join(".mcp.json"));
    assert!(cfg["mcpServers"]["codegraph"].is_object());
}

#[test]
fn claude_install_creates_short_claude_md_guidance() {
    let env = TestEnv::new();
    let claude = get_target("claude").unwrap();
    let result = claude.install(Location::Local, &no_allow());
    let claude_md = env.cwd().join(".claude").join("CLAUDE.md");
    let body = read(&claude_md);
    assert!(body.contains("<!-- CODEGRAPH_START -->"));
    assert!(body.contains("codegraph_explore"));
    assert!(body.contains("codegraph explore \"<symbol names or question>\""));
    assert!(body.contains("If there is no `.codegraph/` directory"));
    assert_eq!(
        result
            .files
            .iter()
            .find(|f| f.path.ends_with("CLAUDE.md"))
            .map(|f| f.action),
        Some(FileAction::Created)
    );
}

#[test]
fn claude_install_replaces_legacy_claude_md_block_keeping_user_content() {
    let env = TestEnv::new();
    let claude = get_target("claude").unwrap();
    let claude_md = env.cwd().join(".claude").join("CLAUDE.md");
    write(
        &claude_md,
        &format!("# My project rules\n\nUse tabs.\n\n{LEGACY_BLOCK}\n"),
    );

    let result = claude.install(Location::Local, &no_allow());

    let body = read(&claude_md);
    assert!(body.contains("# My project rules"));
    assert!(body.contains("Use tabs."));
    assert_eq!(body.matches("CODEGRAPH_START").count(), 1);
    assert!(body.contains("codegraph_explore"));
    assert!(!body.contains("Prefer `codegraph_search`"));
    assert_eq!(
        result
            .files
            .iter()
            .find(|f| f.path.ends_with("CLAUDE.md"))
            .map(|f| f.action),
        Some(FileAction::Updated)
    );
}

#[test]
fn claude_instructions_are_byte_identical_on_reinstall() {
    let env = TestEnv::new();
    let claude = get_target("claude").unwrap();
    let claude_md = env.cwd().join(".claude").join("CLAUDE.md");

    claude.install(Location::Local, &no_allow());
    let first = read(&claude_md);
    let second = claude.install(Location::Local, &no_allow());

    assert_eq!(read(&claude_md), first);
    assert_eq!(
        second
            .files
            .iter()
            .find(|f| f.path.ends_with("CLAUDE.md"))
            .map(|f| f.action),
        Some(FileAction::Unchanged)
    );
}

#[test]
fn claude_global_install_targets_home_claude_json() {
    let env = TestEnv::new();
    let claude = get_target("claude").unwrap();
    claude.install(Location::Global, &no_allow());
    let cfg = read_json(&env.home().join(".claude.json"));
    assert!(cfg["mcpServers"]["codegraph"].is_object());
}

#[test]
fn claude_local_install_migrates_legacy_claude_json_entry() {
    let env = TestEnv::new();
    let claude = get_target("claude").unwrap();
    let legacy = env.cwd().join(".claude.json");
    write(
        &legacy,
        &serde_json::to_string_pretty(&json!({
            "mcpServers": { "codegraph": { "type": "stdio", "command": "codegraph", "args": ["serve", "--mcp"] } }
        }))
        .unwrap(),
    );

    claude.install(Location::Local, &no_allow());

    // codegraph now lives in .mcp.json; the legacy file (which held only
    // codegraph) is gone.
    let mcp = read_json(&env.cwd().join(".mcp.json"));
    assert!(mcp["mcpServers"]["codegraph"].is_object());
    assert!(!legacy.exists());
}

#[test]
fn claude_legacy_migration_preserves_sibling_servers_and_unrelated_keys() {
    let env = TestEnv::new();
    let claude = get_target("claude").unwrap();
    let legacy = env.cwd().join(".claude.json");
    write(
        &legacy,
        &serde_json::to_string_pretty(&json!({
            "mcpServers": {
                "codegraph": { "type": "stdio", "command": "codegraph", "args": ["serve", "--mcp"] },
                "other": { "command": "x" },
            },
            "somethingElse": true,
        }))
        .unwrap(),
    );

    claude.install(Location::Local, &no_allow());

    // Only codegraph is stripped from the legacy file; siblings survive.
    let after = read_json(&legacy);
    assert!(after["mcpServers"].get("codegraph").is_none());
    assert!(after["mcpServers"]["other"].is_object());
    assert_eq!(after["somethingElse"], true);
    let mcp = read_json(&env.cwd().join(".mcp.json"));
    assert!(mcp["mcpServers"]["codegraph"].is_object());
}

#[test]
fn claude_uninstall_strips_codegraph_from_mcp_json_and_legacy_claude_json() {
    let env = TestEnv::new();
    let claude = get_target("claude").unwrap();
    // A user left with both the working .mcp.json and a stale .claude.json.
    write(
        &env.cwd().join(".mcp.json"),
        &serde_json::to_string_pretty(
            &json!({ "mcpServers": { "codegraph": { "command": "codegraph" } } }),
        )
        .unwrap(),
    );
    write(
        &env.cwd().join(".claude.json"),
        &serde_json::to_string_pretty(&json!({
            "mcpServers": { "codegraph": { "command": "codegraph" }, "other": { "command": "x" } }
        }))
        .unwrap(),
    );

    claude.uninstall(Location::Local);

    let mcp = read_json(&env.cwd().join(".mcp.json"));
    assert!(mcp.get("mcpServers").is_none());
    let legacy = read_json(&env.cwd().join(".claude.json"));
    assert!(legacy["mcpServers"].get("codegraph").is_none());
    assert!(legacy["mcpServers"]["other"].is_object());
}

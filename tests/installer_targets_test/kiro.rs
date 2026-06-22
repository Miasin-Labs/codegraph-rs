use crate::support::*;

#[test]
fn kiro_install_writes_mcp_json_and_no_steering_doc() {
    let env = TestEnv::new();
    let kiro = get_target("kiro").unwrap();
    let result = kiro.install(Location::Global, &auto_allow());
    let mcp = env.home().join(".kiro").join("settings").join("mcp.json");
    let steering = env
        .home()
        .join(".kiro")
        .join("steering")
        .join("codegraph.md");
    assert!(result.files.iter().any(|f| f.path == mcp));
    assert!(!result.files.iter().any(|f| f.path == steering));
    assert!(!steering.exists());

    let cfg = read_json(&mcp);
    assert_eq!(
        cfg["mcpServers"]["codegraph"],
        json!({ "type": "stdio", "command": "codegraph", "args": ["serve", "--mcp"] })
    );
}

#[test]
fn kiro_install_deletes_leftover_steering_doc_self_heal() {
    let env = TestEnv::new();
    let kiro = get_target("kiro").unwrap();
    let steering = env
        .home()
        .join(".kiro")
        .join("steering")
        .join("codegraph.md");
    write(&steering, &format!("{LEGACY_BLOCK}\n"));

    let result = kiro.install(Location::Global, &auto_allow());
    assert!(!steering.exists());
    assert_eq!(
        result
            .files
            .iter()
            .find(|f| f.path == steering)
            .map(|f| f.action),
        Some(FileAction::Removed)
    );
}

#[test]
fn kiro_install_preserves_sibling_mcp_server() {
    let env = TestEnv::new();
    let kiro = get_target("kiro").unwrap();
    let mcp = env.home().join(".kiro").join("settings").join("mcp.json");
    write(
        &mcp,
        &pretty(
            &json!({ "mcpServers": { "other": { "command": "uvx", "args": ["other-server"] } } }),
        ),
    );

    kiro.install(Location::Global, &auto_allow());

    let after = read_json(&mcp);
    assert!(after["mcpServers"]["other"].is_object());
    assert!(after["mcpServers"]["codegraph"].is_object());
}

#[test]
fn kiro_uninstall_strips_codegraph_but_leaves_siblings() {
    let env = TestEnv::new();
    let kiro = get_target("kiro").unwrap();
    let mcp = env.home().join(".kiro").join("settings").join("mcp.json");
    write(
        &mcp,
        &pretty(
            &json!({ "mcpServers": { "other": { "command": "uvx", "args": ["other-server"] } } }),
        ),
    );

    kiro.install(Location::Global, &auto_allow());
    kiro.uninstall(Location::Global);

    let after = read_json(&mcp);
    assert!(after["mcpServers"]["other"].is_object());
    assert!(after["mcpServers"].get("codegraph").is_none());
}

#[test]
fn kiro_uninstall_removes_leftover_steering_doc_outright() {
    let env = TestEnv::new();
    let kiro = get_target("kiro").unwrap();
    let steering = env
        .home()
        .join(".kiro")
        .join("steering")
        .join("codegraph.md");
    write(&steering, &format!("{LEGACY_BLOCK}\n"));

    kiro.uninstall(Location::Global);
    assert!(!steering.exists());
}

#[test]
fn kiro_uninstall_leaves_sibling_steering_doc_untouched() {
    let env = TestEnv::new();
    let kiro = get_target("kiro").unwrap();
    let sibling = env.home().join(".kiro").join("steering").join("product.md");
    let ours = env
        .home()
        .join(".kiro")
        .join("steering")
        .join("codegraph.md");
    write(&sibling, "# Product\n\nMy team practices.\n");
    write(&ours, &format!("{LEGACY_BLOCK}\n"));

    kiro.uninstall(Location::Global);

    assert!(!ours.exists());
    assert!(sibling.exists());
    assert!(read(&sibling).contains("My team practices."));
}

#[test]
fn kiro_local_install_writes_local_mcp_json_and_no_steering_doc() {
    let _env = TestEnv::new();
    let kiro = get_target("kiro").unwrap();
    let result = kiro.install(Location::Local, &auto_allow());
    let paths: Vec<String> = result
        .files
        .iter()
        .map(|f| f.path.to_string_lossy().replace('\\', "/"))
        .collect();
    assert!(
        paths
            .iter()
            .any(|p| p.ends_with("/.kiro/settings/mcp.json"))
    );
    assert!(
        !paths
            .iter()
            .any(|p| p.ends_with("/.kiro/steering/codegraph.md"))
    );
}

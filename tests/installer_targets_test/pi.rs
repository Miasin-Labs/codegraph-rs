use crate::support::*;

fn target() -> &'static dyn AgentTarget {
    get_target("pi").unwrap()
}

fn global_file(env: &TestEnv) -> PathBuf {
    env.home().join(".pi/agent/settings.json")
}

fn local_file(env: &TestEnv) -> PathBuf {
    env.cwd().join(".pi/agent/settings.json")
}

#[test]
fn pi_global_install_writes_mcp_servers_shape() {
    let env = TestEnv::new();
    let result = target().install(Location::Global, &auto_allow());
    assert_eq!(result.files[0].path, global_file(&env));
    assert_eq!(result.files[0].action, FileAction::Created);
    let config = read_json(&result.files[0].path);
    assert_eq!(
        config["mcpServers"]["codegraph"],
        json!({"type":"stdio","command":"codegraph","args":["serve","--mcp"]})
    );
}

#[test]
fn pi_local_install_targets_project_dir() {
    let env = TestEnv::new();
    assert!(target().supports_location(Location::Local));
    let result = target().install(Location::Local, &auto_allow());
    assert_eq!(result.files[0].path, local_file(&env));
    assert!(read_json(&local_file(&env))["mcpServers"]["codegraph"].is_object());
}

#[test]
fn pi_honors_env_dir_override_for_global() {
    let env = TestEnv::new();
    let relocated = env.home().join("relocated-pi");
    env::set_var("PI_CODING_AGENT_DIR", &relocated);
    let expected = relocated.join("settings.json");
    assert_eq!(
        target().describe_paths(Location::Global),
        vec![expected.clone()]
    );
    assert_eq!(
        target().install(Location::Global, &auto_allow()).files[0].path,
        expected
    );
    env::remove_var("PI_CODING_AGENT_DIR");
}

#[test]
fn pi_preserves_sibling_settings_and_servers() {
    let env = TestEnv::new();
    let file = global_file(&env);
    write(
        &file,
        &pretty(&json!({
            "defaultModel": "anthropic/claude-opus-5",
            "mcpServers": { "other": { "type": "stdio", "command": "other-server" } }
        })),
    );
    target().install(Location::Global, &auto_allow());
    let config = read_json(&file);
    assert_eq!(config["defaultModel"], "anthropic/claude-opus-5");
    assert!(config["mcpServers"]["other"].is_object());
    assert!(config["mcpServers"]["codegraph"].is_object());
}

#[test]
fn pi_reinstall_is_unchanged() {
    let _env = TestEnv::new();
    target().install(Location::Global, &auto_allow());
    assert_eq!(
        target().install(Location::Global, &auto_allow()).files[0].action,
        FileAction::Unchanged
    );
}

#[test]
fn pi_uninstall_drops_entry_keeps_siblings_and_file() {
    let env = TestEnv::new();
    let file = global_file(&env);
    write(
        &file,
        &pretty(&json!({
            "defaultModel": "x",
            "mcpServers": { "other": { "command": "other-server" } }
        })),
    );
    target().install(Location::Global, &auto_allow());
    assert_eq!(
        target().uninstall(Location::Global).files[0].action,
        FileAction::Removed
    );
    let config = read_json(&file);
    assert_eq!(config["defaultModel"], "x");
    assert!(config["mcpServers"]["other"].is_object());
    assert!(config["mcpServers"].get("codegraph").is_none());
}

#[test]
fn pi_uninstall_handles_absence() {
    let _env = TestEnv::new();
    assert_eq!(
        target().uninstall(Location::Global).files[0].action,
        FileAction::NotFound
    );
}

#[test]
fn pi_detection_and_print_config_match_source() {
    let env = TestEnv::new();
    assert!(!target().detect(Location::Global).installed);
    fs::create_dir_all(env.home().join(".pi/agent")).unwrap();
    assert!(target().detect(Location::Global).installed);
    assert!(!target().detect(Location::Global).already_configured);

    let output = target().print_config(Location::Global);
    let printed: Value = serde_json::from_str(&output[output.find('{').unwrap()..]).unwrap();
    let result = target().install(Location::Global, &auto_allow());
    assert_eq!(
        printed["mcpServers"]["codegraph"],
        read_json(&result.files[0].path)["mcpServers"]["codegraph"]
    );
    assert!(target().detect(Location::Global).already_configured);
    assert!(result.notes.join(" ").contains("Restart"));
}

use crate::support::*;

fn target() -> &'static dyn AgentTarget {
    get_target("copilot-cli").unwrap()
}

fn config_file(env: &TestEnv) -> PathBuf {
    env.home().join(".copilot/mcp-config.json")
}

#[test]
fn copilot_cli_global_install_writes_documented_entry_shape() {
    let env = TestEnv::new();
    let result = target().install(Location::Global, &auto_allow());
    assert_eq!(result.files[0].path, config_file(&env));
    assert_eq!(result.files[0].action, FileAction::Created);
    assert_eq!(
        read_json(&result.files[0].path)["mcpServers"]["codegraph"],
        json!({"type":"stdio","command":"codegraph","args":["serve","--mcp"],"tools":["*"]})
    );
}

#[test]
fn copilot_cli_is_global_only_with_clear_local_semantics() {
    let _env = TestEnv::new();
    assert!(target().supports_location(Location::Global));
    assert!(!target().supports_location(Location::Local));
    let result = target().install(Location::Local, &auto_allow());
    assert!(result.files.is_empty());
    assert!(result.notes.join(" ").contains("no project-local config"));
    assert!(target().uninstall(Location::Local).files.is_empty());
    assert!(target().describe_paths(Location::Local).is_empty());
    assert!(!target().detect(Location::Local).installed);
}

#[test]
fn copilot_cli_honors_copilot_home_for_full_lifecycle() {
    let env = TestEnv::new();
    let custom = env.home().join("copilot-custom");
    env::set_var("COPILOT_HOME", &custom);
    let result = target().install(Location::Global, &auto_allow());
    assert_eq!(result.files[0].path, custom.join("mcp-config.json"));
    assert!(target().detect(Location::Global).already_configured);
    assert!(!env.home().join(".copilot").exists());
    target().uninstall(Location::Global);
    assert!(!target().detect(Location::Global).already_configured);
}

#[test]
fn copilot_cli_uninstall_preserves_siblings_and_unrelated_top_level_keys() {
    let env = TestEnv::new();
    let file = config_file(&env);
    write(
        &file,
        &pretty(&json!({
            "mcpServers":{"other":{"command":"other-server"}},
            "banner":"never"
        })),
    );
    target().install(Location::Global, &auto_allow());
    target().uninstall(Location::Global);
    let config = read_json(&file);
    assert!(config["mcpServers"]["other"].is_object());
    assert!(config["mcpServers"].get("codegraph").is_none());
    assert_eq!(config["banner"], "never");
}

#[test]
fn copilot_cli_uninstall_deletes_only_empty_from_scratch_file() {
    let env = TestEnv::new();
    let file = config_file(&env);
    target().install(Location::Global, &auto_allow());
    target().uninstall(Location::Global);
    assert!(!file.exists());

    write(&file, &pretty(&json!({"banner":"never"})));
    target().install(Location::Global, &auto_allow());
    target().uninstall(Location::Global);
    assert_eq!(read_json(&file)["banner"], "never");
    assert!(read_json(&file).get("mcpServers").is_none());
}

#[test]
fn copilot_cli_uninstall_absence_and_detection_heuristics_match_source() {
    let env = TestEnv::new();
    assert_eq!(
        target().uninstall(Location::Global).files[0].action,
        FileAction::NotFound
    );
    let file = config_file(&env);
    write(
        &file,
        &pretty(&json!({"mcpServers":{"other":{"command":"x"}}})),
    );
    assert_eq!(
        target().uninstall(Location::Global).files[0].action,
        FileAction::NotFound
    );
    fs::remove_file(&file).unwrap();
    write(&env.home().join(".copilot/config.json"), "{}");
    assert!(target().detect(Location::Global).installed);
    assert!(!target().detect(Location::Global).already_configured);
}

#[test]
fn copilot_cli_detection_ignores_ide_only_directory_and_print_matches_install() {
    let env = TestEnv::new();
    let ide = env.home().join(".copilot/ide");
    write(&ide.join("some-uuid.lock"), "{}");
    let previous = env::var_os("PATH");
    env::set_var("PATH", &ide);
    assert!(!target().detect(Location::Global).installed);
    fs::remove_dir_all(&ide).unwrap();
    assert!(!target().detect(Location::Global).installed);
    match previous {
        Some(value) => env::set_var("PATH", value),
        None => env::remove_var("PATH"),
    }

    let output = target().print_config(Location::Global);
    let printed: Value = serde_json::from_str(&output[output.find('{').unwrap()..]).unwrap();
    let result = target().install(Location::Global, &auto_allow());
    assert_eq!(
        printed["mcpServers"]["codegraph"],
        read_json(&result.files[0].path)["mcpServers"]["codegraph"]
    );
    assert!(
        target()
            .print_config(Location::Local)
            .contains("--location=global")
    );
}

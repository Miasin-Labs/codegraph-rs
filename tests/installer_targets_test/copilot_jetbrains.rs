use crate::support::*;

fn target() -> &'static dyn AgentTarget {
    get_target("copilot-jetbrains").unwrap()
}

fn config_file(env: &TestEnv) -> PathBuf {
    env.home().join(".config/github-copilot/intellij/mcp.json")
}

#[test]
fn copilot_jetbrains_global_install_writes_vscode_compatible_shape() {
    let env = TestEnv::new();
    let result = target().install(Location::Global, &auto_allow());
    assert_eq!(result.files[0].path, config_file(&env));
    assert_eq!(result.files[0].action, FileAction::Created);
    let config = read_json(&result.files[0].path);
    assert_eq!(
        config["servers"]["codegraph"],
        json!({"type":"stdio","command":"codegraph","args":["serve","--mcp"]})
    );
    assert!(config.get("mcpServers").is_none());
}

#[cfg(not(windows))]
#[test]
fn copilot_jetbrains_falls_back_to_home_config_without_xdg() {
    let env = TestEnv::new();
    env::remove_var("XDG_CONFIG_HOME");
    let expected = env.home().join(".config/github-copilot/intellij/mcp.json");
    assert_eq!(
        target().describe_paths(Location::Global),
        vec![expected.clone()]
    );
    assert_eq!(
        target().install(Location::Global, &auto_allow()).files[0].path,
        expected
    );
}

#[cfg(windows)]
#[test]
fn copilot_jetbrains_falls_back_to_localappdata_without_xdg() {
    let env = TestEnv::new();
    env::remove_var("XDG_CONFIG_HOME");
    let root = env.home().join("AppData/Local");
    env::set_var("LOCALAPPDATA", &root);
    let expected = root.join("github-copilot/intellij/mcp.json");
    assert_eq!(target().describe_paths(Location::Global), vec![expected]);
}

#[test]
fn copilot_jetbrains_is_global_only_with_clear_local_semantics() {
    let _env = TestEnv::new();
    assert!(target().supports_location(Location::Global));
    assert!(!target().supports_location(Location::Local));
    let result = target().install(Location::Local, &auto_allow());
    assert!(result.files.is_empty());
    assert!(
        result
            .notes
            .join(" ")
            .contains("no project-local MCP config")
    );
    assert!(target().uninstall(Location::Local).files.is_empty());
    assert!(target().describe_paths(Location::Local).is_empty());
    assert!(!target().detect(Location::Local).installed);
}

#[test]
fn copilot_jetbrains_preserves_jsonc_comments_siblings_and_reinstall_bytes() {
    let env = TestEnv::new();
    let file = config_file(&env);
    write(
        &file,
        "{\n  // hand-edited via Settings → Tools → GitHub Copilot\n  \"servers\": {\n    \"other\": { \"type\": \"stdio\", \"command\": \"other-server\" }\n  }\n}\n",
    );
    target().install(Location::Global, &auto_allow());
    let installed = read(&file);
    assert!(installed.contains("// hand-edited via Settings"));
    assert!(installed.contains("\"other-server\""));
    assert!(installed.contains("\"codegraph\""));
    assert_eq!(
        target().install(Location::Global, &auto_allow()).files[0].action,
        FileAction::Unchanged
    );
    assert_eq!(read(&file), installed);
}

#[test]
fn copilot_jetbrains_uninstall_drops_empty_wrapper_but_keeps_file() {
    let env = TestEnv::new();
    let file = config_file(&env);
    target().install(Location::Global, &auto_allow());
    assert_eq!(
        target().uninstall(Location::Global).files[0].action,
        FileAction::Removed
    );
    assert!(file.exists());
    assert!(!read(&file).contains("\"servers\""));
}

#[test]
fn copilot_jetbrains_uninstall_keeps_sibling_and_handles_absence() {
    let env = TestEnv::new();
    assert_eq!(
        target().uninstall(Location::Global).files[0].action,
        FileAction::NotFound
    );
    let file = config_file(&env);
    write(
        &file,
        &pretty(&json!({"servers":{"other":{"command":"other-server"}}})),
    );
    target().install(Location::Global, &auto_allow());
    target().uninstall(Location::Global);
    let config = read_json(&file);
    assert!(config["servers"]["other"].is_object());
    assert!(config["servers"].get("codegraph").is_none());
}

#[test]
fn copilot_jetbrains_detection_print_config_note_and_family_coexistence_match_source() {
    let env = TestEnv::new();
    assert!(!target().detect(Location::Global).installed);
    fs::create_dir_all(env.home().join(".config/github-copilot/intellij")).unwrap();
    assert!(target().detect(Location::Global).installed);
    assert!(!target().detect(Location::Global).already_configured);

    let output = target().print_config(Location::Global);
    assert!(output.contains("Settings → Tools → GitHub Copilot"));
    let printed: Value = serde_json::from_str(&output[output.find('{').unwrap()..]).unwrap();
    let result = target().install(Location::Global, &auto_allow());
    assert_eq!(
        printed["servers"]["codegraph"],
        read_json(&result.files[0].path)["servers"]["codegraph"]
    );
    assert!(
        target()
            .print_config(Location::Local)
            .contains("--location=global")
    );
    assert!(
        result
            .notes
            .join(" ")
            .contains("Restart your JetBrains IDE")
    );

    let vscode = get_target("copilot-vscode").unwrap();
    let cli = get_target("copilot-cli").unwrap();
    vscode.install(Location::Global, &auto_allow());
    cli.install(Location::Global, &auto_allow());
    cli.uninstall(Location::Global);
    assert!(!cli.detect(Location::Global).already_configured);
    assert!(vscode.detect(Location::Global).already_configured);
    assert!(target().detect(Location::Global).already_configured);
}

use crate::support::*;

fn target() -> &'static dyn AgentTarget {
    get_target("copilot-vscode").unwrap()
}

fn local_file(env: &TestEnv) -> PathBuf {
    env.cwd().join(".vscode").join("mcp.json")
}

fn printed_json(output: &str) -> Value {
    serde_json::from_str(&output[output.find('{').unwrap()..]).unwrap()
}

#[test]
fn copilot_vscode_local_install_writes_absolute_path_entry() {
    let env = TestEnv::new();
    let result = target().install(Location::Local, &auto_allow());
    let file = local_file(&env);
    assert_eq!(result.files[0].path, file);
    assert_eq!(result.files[0].action, FileAction::Created);
    let config = read_json(&file);
    assert_eq!(config["servers"]["codegraph"]["type"], "stdio");
    assert_eq!(config["servers"]["codegraph"]["command"], "codegraph");
    assert_eq!(
        config["servers"]["codegraph"]["args"],
        json!(["serve", "--mcp", "--path", env.cwd()])
    );
    assert!(config.get("mcpServers").is_none());
}

#[test]
fn copilot_vscode_global_install_is_variable_and_path_free() {
    let _env = TestEnv::new();
    let result = target().install(Location::Global, &auto_allow());
    let config = read_json(&result.files[0].path);
    assert_eq!(
        config["servers"]["codegraph"]["args"],
        json!(["serve", "--mcp"])
    );
    assert!(!config.to_string().contains("${"));
}

#[cfg(target_os = "linux")]
#[test]
fn copilot_vscode_global_path_honors_xdg_config_home() {
    let env = TestEnv::new();
    let expected = env.home().join(".config/Code/User/mcp.json");
    assert_eq!(
        target().describe_paths(Location::Global),
        vec![expected.clone()]
    );
    assert_eq!(
        target().install(Location::Global, &auto_allow()).files[0].path,
        expected
    );
}

#[cfg(target_os = "macos")]
#[test]
fn copilot_vscode_global_path_uses_macos_code_user_dir() {
    let env = TestEnv::new();
    let expected = env
        .home()
        .join("Library/Application Support/Code/User/mcp.json");
    assert_eq!(target().describe_paths(Location::Global), vec![expected]);
}

#[cfg(windows)]
#[test]
fn copilot_vscode_global_path_uses_appdata_code_user_dir() {
    let env = TestEnv::new();
    let expected = env.home().join(".config/Code/User/mcp.json");
    assert_eq!(target().describe_paths(Location::Global), vec![expected]);
}

#[test]
fn copilot_vscode_supports_global_and_local() {
    let _env = TestEnv::new();
    assert!(target().supports_location(Location::Global));
    assert!(target().supports_location(Location::Local));
}

#[test]
fn copilot_vscode_preserves_jsonc_comments_siblings_and_reinstall_bytes() {
    let env = TestEnv::new();
    let file = local_file(&env);
    write(
        &file,
        "{\n  // my MCP servers\n  \"servers\": {\n    \"other\": { \"type\": \"stdio\", \"command\": \"other-server\" } // keep\n  }\n}\n",
    );
    target().install(Location::Local, &auto_allow());
    let installed = read(&file);
    assert!(installed.contains("// my MCP servers"));
    assert!(installed.contains("// keep"));
    assert!(installed.contains("\"other-server\""));
    assert!(installed.contains("\"codegraph\""));
    assert_eq!(
        target().install(Location::Local, &auto_allow()).files[0].action,
        FileAction::Unchanged
    );
    assert_eq!(read(&file), installed);
}

#[test]
fn copilot_vscode_uninstall_drops_empty_servers_but_preserves_inputs_and_comments() {
    let env = TestEnv::new();
    let file = local_file(&env);
    write(
        &file,
        "{\n  // prompt-time inputs\n  \"inputs\": [{ \"id\": \"api-key\", \"type\": \"promptString\" }]\n}\n",
    );
    target().install(Location::Local, &auto_allow());
    assert_eq!(
        target().uninstall(Location::Local).files[0].action,
        FileAction::Removed
    );
    let text = read(&file);
    assert!(text.contains("// prompt-time inputs"));
    assert!(text.contains("\"inputs\""));
    assert!(!text.contains("\"servers\""));
    assert!(!text.contains("codegraph"));
}

#[test]
fn copilot_vscode_uninstall_keeps_sibling_server_and_handles_absence() {
    let env = TestEnv::new();
    assert_eq!(
        target().uninstall(Location::Global).files[0].action,
        FileAction::NotFound
    );
    assert_eq!(
        target().uninstall(Location::Local).files[0].action,
        FileAction::NotFound
    );
    let file = local_file(&env);
    write(
        &file,
        &pretty(&json!({"servers":{"other":{"command":"other-server"}}})),
    );
    target().install(Location::Local, &auto_allow());
    target().uninstall(Location::Local);
    let config = read_json(&file);
    assert!(config["servers"]["other"].is_object());
    assert!(config["servers"].get("codegraph").is_none());
}

#[test]
fn copilot_vscode_detection_print_config_and_restart_note_match_source() {
    let env = TestEnv::new();
    assert!(!target().detect(Location::Local).installed);
    fs::create_dir_all(env.cwd().join(".vscode")).unwrap();
    assert!(target().detect(Location::Local).installed);
    assert!(!target().detect(Location::Local).already_configured);
    assert!(!target().detect(Location::Global).installed);
    fs::create_dir_all(env.home().join(".vscode")).unwrap();
    assert!(target().detect(Location::Global).installed);

    for location in [Location::Global, Location::Local] {
        let printed = printed_json(&target().print_config(location));
        let result = target().install(location, &auto_allow());
        assert_eq!(
            printed["servers"]["codegraph"],
            read_json(&result.files[0].path)["servers"]["codegraph"]
        );
    }
    assert!(
        target()
            .install(Location::Local, &auto_allow())
            .notes
            .join(" ")
            .contains("Restart VS Code")
    );
}

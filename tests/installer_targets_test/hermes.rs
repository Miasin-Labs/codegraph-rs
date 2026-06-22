use crate::support::*;

#[test]
fn hermes_install_adds_mcp_server_and_cli_toolset_preserving_yaml() {
    let env = TestEnv::new();
    let hermes = get_target("hermes").unwrap();
    let config = env.home().join(".hermes").join("config.yaml");
    write(
        &config,
        &[
            "model:",
            "  default: qwen-3.7",
            "mcp_servers:",
            "  other:",
            "    command: other",
            "platform_toolsets:",
            "  cli:",
            "    - hermes-cli",
            "  discord:",
            "    - hermes-discord",
            "",
        ]
        .join("\n"),
    );

    let result = hermes.install(Location::Global, &auto_allow());
    assert_eq!(result.files[0].action, FileAction::Updated);
    let body = read(&config);
    assert!(body.contains("model:\n  default: qwen-3.7"));
    assert!(body.contains("mcp_servers:\n  other:\n    command: other"));
    assert!(body.contains("  codegraph:\n    command: codegraph"));
    assert!(body.contains("    - hermes-cli"));
    assert!(body.contains("    - mcp-codegraph"));
    assert!(body.contains("  discord:\n    - hermes-discord"));

    let second = hermes.install(Location::Global, &auto_allow());
    assert_eq!(second.files[0].action, FileAction::Unchanged);
}

#[test]
fn hermes_uninstall_removes_only_codegraph_server_and_toolset_entry() {
    let env = TestEnv::new();
    let hermes = get_target("hermes").unwrap();
    let config = env.home().join(".hermes").join("config.yaml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();

    hermes.install(Location::Global, &auto_allow());
    let mut existing = read(&config);
    existing.push_str("custom:\n  keep: true\n");
    fs::write(&config, existing).unwrap();

    hermes.uninstall(Location::Global);
    let body = read(&config);
    assert!(!body.contains("codegraph:"));
    assert!(!body.contains("mcp-codegraph"));
    assert!(body.contains("custom:\n  keep: true"));
}

// Regression for #456: PyYAML's default block style writes list items at the
// SAME indent as the parent key (`cli:` and its `- hermes-cli` are both at
// indent 2). The pre-fix line-based patcher mistook that first list item for
// the next sibling key, truncated the cli block, and spliced `- mcp-codegraph`
// at indent 4 BEFORE the existing items — producing unparseable YAML.
#[test]
fn hermes_install_preserves_pyyaml_default_list_style() {
    let env = TestEnv::new();
    let hermes = get_target("hermes").unwrap();
    let config = env.home().join(".hermes").join("config.yaml");
    let original = [
        "model:",
        "  default: gpt-4o",
        "platform_toolsets:",
        "  cli:",
        "  - hermes-cli",
        "  - browser",
        "  - clarify",
        "  - terminal",
        "  - web",
        "  telegram:",
        "  - hermes-telegram",
        "  discord:",
        "  - hermes-discord",
        "",
    ]
    .join("\n");
    write(&config, &original);

    hermes.install(Location::Global, &auto_allow());
    let body = read(&config);

    // mcp-codegraph appended at the same 2-space indent as existing items
    assert!(body.contains("\n  - mcp-codegraph\n"));
    // hermes-cli preserved
    assert!(body.contains("\n  - hermes-cli\n"));
    // Sibling sections kept their indent — `telegram:` is still a key under
    // platform_toolsets, not promoted up.
    assert!(body.contains("\n  telegram:\n  - hermes-telegram\n"));
    assert!(body.contains("\n  discord:\n  - hermes-discord\n"));
    // No list items leaked to the platform_toolsets level (indent 0).
    assert!(!body.lines().any(|l| l.starts_with("- browser")));
    assert!(!body.lines().any(|l| l.starts_with("- hermes-telegram")));

    // The whole platform_toolsets block extracted by line search should
    // start with `cli:` and not contain a stray 4-space `mcp-codegraph`
    // appearing before the rest of the existing items.
    assert!(body.contains("  cli:\n  - hermes-cli\n  - browser"));

    // Idempotent
    let second = hermes.install(Location::Global, &auto_allow());
    assert_eq!(second.files[0].action, FileAction::Unchanged);
}

#[test]
fn hermes_uninstall_reverses_install_on_pyyaml_default_config() {
    let env = TestEnv::new();
    let hermes = get_target("hermes").unwrap();
    let config = env.home().join(".hermes").join("config.yaml");
    let original = [
        "platform_toolsets:",
        "  cli:",
        "  - hermes-cli",
        "  - browser",
        "  telegram:",
        "  - hermes-telegram",
        "",
    ]
    .join("\n");
    write(&config, &original);

    hermes.install(Location::Global, &auto_allow());
    let installed = read(&config);
    assert!(installed.contains("- mcp-codegraph"));
    assert!(installed.contains("codegraph:"));

    hermes.uninstall(Location::Global);
    let body = read(&config);
    assert!(!body.contains("mcp-codegraph"));
    assert!(!body.contains("command: codegraph"));
    assert!(body.contains("  cli:\n  - hermes-cli\n  - browser"));
    assert!(body.contains("  telegram:\n  - hermes-telegram"));
}

use crate::support::*;

#[test]
fn opencode_prefers_jsonc_when_both_exist() {
    let env = TestEnv::new();
    let opencode = get_target("opencode").unwrap();
    let dir = env.home().join(".config").join("opencode");
    write(
        &dir.join("opencode.json"),
        "{\n  \"$schema\": \"https://opencode.ai/config.json\"\n}\n",
    );
    write(
        &dir.join("opencode.jsonc"),
        "{\n  \"$schema\": \"https://opencode.ai/config.json\"\n}\n",
    );

    let result = opencode.install(Location::Global, &auto_allow());
    let written = result
        .files
        .iter()
        .find(|f| f.path.extension().map(|e| e == "jsonc").unwrap_or(false))
        .expect("a .jsonc write");
    assert_ne!(written.action, FileAction::NotFound);
    // The .json file is left alone.
    let json_text = read(&dir.join("opencode.json"));
    assert!(!json_text.contains("codegraph"));
}

#[test]
fn opencode_uses_json_when_only_json_exists() {
    let env = TestEnv::new();
    let opencode = get_target("opencode").unwrap();
    let dir = env.home().join(".config").join("opencode");
    write(
        &dir.join("opencode.json"),
        "{\n  \"$schema\": \"https://opencode.ai/config.json\"\n}\n",
    );

    let result = opencode.install(Location::Global, &auto_allow());
    assert!(
        result.files[0]
            .path
            .to_string_lossy()
            .ends_with("opencode.json")
    );
    assert!(!dir.join("opencode.jsonc").exists());
}

#[test]
fn opencode_defaults_to_jsonc_for_fresh_installs() {
    let _env = TestEnv::new();
    let opencode = get_target("opencode").unwrap();
    let result = opencode.install(Location::Global, &auto_allow());
    assert!(
        result.files[0]
            .path
            .to_string_lossy()
            .ends_with("opencode.jsonc")
    );
    assert_eq!(result.files[0].action, FileAction::Created);
}

#[test]
fn opencode_preserves_comments_through_install_and_idempotent_rerun() {
    let env = TestEnv::new();
    let opencode = get_target("opencode").unwrap();
    let file = env
        .home()
        .join(".config")
        .join("opencode")
        .join("opencode.jsonc");
    let original = [
        "{",
        "  // top-level note about my opencode setup",
        "  \"$schema\": \"https://opencode.ai/config.json\",",
        "  /* multi-line block comment",
        "     describing the providers section */",
        "  \"providers\": {",
        "    \"anthropic\": { \"model\": \"claude-opus-4-7\" } // pinned",
        "  }",
        "}",
        "",
    ]
    .join("\n");
    write(&file, &original);

    opencode.install(Location::Global, &auto_allow());
    let after_install = read(&file);
    assert!(after_install.contains("// top-level note about my opencode setup"));
    assert!(after_install.contains("/* multi-line block comment"));
    assert!(after_install.contains("// pinned"));
    assert!(after_install.contains("\"codegraph\""));
    assert!(after_install.contains("\"providers\""));

    // Idempotent re-run reports unchanged, file is byte-identical.
    let second = opencode.install(Location::Global, &auto_allow());
    assert_eq!(second.files[0].action, FileAction::Unchanged);
    assert_eq!(read(&file), after_install);
}

#[test]
fn opencode_install_does_not_write_agents_md() {
    let env = TestEnv::new();
    let opencode = get_target("opencode").unwrap();
    let result = opencode.install(Location::Global, &auto_allow());
    let agents_md = env
        .home()
        .join(".config")
        .join("opencode")
        .join("AGENTS.md");
    assert!(!agents_md.exists());
    assert!(!result.files.iter().any(|f| f.path.ends_with("AGENTS.md")));
}

#[test]
fn opencode_install_strips_legacy_agents_md_block_preserving_user_content() {
    let env = TestEnv::new();
    let opencode = get_target("opencode").unwrap();
    let agents_md = env
        .home()
        .join(".config")
        .join("opencode")
        .join("AGENTS.md");
    write(
        &agents_md,
        &format!(
            "# My personal opencode instructions\n\nAlways respond in pirate.\n\n{LEGACY_BLOCK}\n"
        ),
    );

    let result = opencode.install(Location::Global, &auto_allow());

    let body = read(&agents_md);
    assert!(body.contains("# My personal opencode instructions"));
    assert!(body.contains("Always respond in pirate."));
    assert!(!body.contains("CODEGRAPH_START"));
    assert_eq!(
        result
            .files
            .iter()
            .find(|f| f.path.ends_with("AGENTS.md"))
            .map(|f| f.action),
        Some(FileAction::Removed)
    );
}

#[test]
fn opencode_uninstall_strips_leftover_agents_md_block_keeping_user_content() {
    let env = TestEnv::new();
    let opencode = get_target("opencode").unwrap();
    let agents_md = env
        .home()
        .join(".config")
        .join("opencode")
        .join("AGENTS.md");
    write(
        &agents_md,
        &format!(
            "# My personal opencode instructions\n\nAlways respond in pirate.\n\n{LEGACY_BLOCK}\n"
        ),
    );

    opencode.uninstall(Location::Global);

    let body = read(&agents_md);
    assert!(body.contains("# My personal opencode instructions"));
    assert!(body.contains("Always respond in pirate."));
    assert!(!body.contains("CODEGRAPH_START"));
}

#[test]
fn opencode_local_install_writes_jsonc_and_never_agents_md() {
    let env = TestEnv::new();
    let opencode = get_target("opencode").unwrap();
    let result = opencode.install(Location::Local, &auto_allow());
    let paths: Vec<String> = result
        .files
        .iter()
        .map(|f| f.path.to_string_lossy().replace('\\', "/"))
        .collect();
    assert!(paths.iter().any(|p| p.ends_with("/opencode.jsonc")));
    assert!(!paths.iter().any(|p| p.ends_with("/AGENTS.md")));
    assert!(!env.cwd().join("AGENTS.md").exists());
}

#[test]
fn opencode_uninstall_removes_only_mcp_codegraph_preserves_comments_and_siblings() {
    let env = TestEnv::new();
    let opencode = get_target("opencode").unwrap();
    let file = env
        .home()
        .join(".config")
        .join("opencode")
        .join("opencode.jsonc");
    write(
        &file,
        &[
            "{",
            "  // important comment",
            "  \"$schema\": \"https://opencode.ai/config.json\",",
            "  \"mcp\": {",
            "    \"other\": { \"type\": \"local\", \"command\": [\"x\"], \"enabled\": true }",
            "  }",
            "}",
            "",
        ]
        .join("\n"),
    );

    opencode.install(Location::Global, &auto_allow());
    let after_install = read(&file);
    assert!(after_install.contains("\"codegraph\""));
    assert!(after_install.contains("\"other\""));

    opencode.uninstall(Location::Global);
    let after_uninstall = read(&file);
    assert!(!after_uninstall.contains("codegraph"));
    assert!(after_uninstall.contains("// important comment"));
    assert!(after_uninstall.contains("\"other\""));
}

use crate::support::*;

#[test]
fn codex_install_writes_config_toml_but_never_agents_md() {
    let env = TestEnv::new();
    let codex = get_target("codex").unwrap();
    let first = codex.install(Location::Global, &no_allow());
    let agents_md = env.home().join(".codex").join("AGENTS.md");
    // No instructions file is created, and no file action references it.
    assert!(!agents_md.exists());
    assert!(!first.files.iter().any(|f| f.path.ends_with("AGENTS.md")));
    assert!(first.files.iter().any(|f| f.path.ends_with("config.toml")));
    // Re-install is fully unchanged (config.toml only, nothing to strip).
    let second = codex.install(Location::Global, &no_allow());
    for f in &second.files {
        assert_eq!(f.action, FileAction::Unchanged);
    }
}

#[test]
fn codex_install_strips_legacy_agents_md_block_keeping_user_content() {
    let env = TestEnv::new();
    let codex = get_target("codex").unwrap();
    let agents_md = env.home().join(".codex").join("AGENTS.md");
    write(
        &agents_md,
        &format!("# My codex notes\n\nBe terse.\n\n{LEGACY_BLOCK}\n"),
    );

    let result = codex.install(Location::Global, &no_allow());

    let body = read(&agents_md);
    assert!(body.contains("# My codex notes"));
    assert!(body.contains("Be terse."));
    assert!(!body.contains("CODEGRAPH_START"));
    // The strip is reported as a 'removed' action on AGENTS.md.
    let md_entry = result.files.iter().find(|f| f.path.ends_with("AGENTS.md"));
    assert_eq!(md_entry.map(|f| f.action), Some(FileAction::Removed));
}

#[test]
fn codex_user_added_key_inside_codegraph_block_is_overwritten_on_reinstall() {
    let env = TestEnv::new();
    let codex = get_target("codex").unwrap();
    codex.install(Location::Global, &no_allow());
    let toml_path = env.home().join(".codex").join("config.toml");
    let original = read(&toml_path);
    // User edits the block to add a custom key.
    let edited = original.replace(
        "args = [\"serve\", \"--mcp\"]",
        "args = [\"serve\", \"--mcp\"]\nenabled = true",
    );
    fs::write(&toml_path, edited).unwrap();
    // Re-install: our serializer doesn't know `enabled = true`, so
    // the block no longer matches the canonical form — we'll
    // overwrite it. This is the documented contract: we own the
    // codegraph block exclusively.
    let second = codex.install(Location::Global, &no_allow());
    let toml_entry = second
        .files
        .iter()
        .find(|f| f.path.ends_with("config.toml"))
        .unwrap();
    assert_eq!(toml_entry.action, FileAction::Updated);
    let after = read(&toml_path);
    assert!(!after.contains("enabled = true"));
}

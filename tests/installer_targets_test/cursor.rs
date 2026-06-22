use crate::support::*;

// ===========================================================================
// Installer — Cursor rules file cleanup on uninstall
// ===========================================================================

// The frontmatter a previous install wrote ahead of the marked block.
// `remove_rules_entry` recognizes it to decide whether the leftover .mdc
// is ours-to-delete or carries user content worth keeping.
const MDC_FRONTMATTER: &str = "---\ndescription: CodeGraph MCP usage guide — when to use which tool\nalwaysApply: true\n---\n";

fn rules_file(env: &TestEnv) -> PathBuf {
    env.cwd()
        .join(".cursor")
        .join("rules")
        .join("codegraph.mdc")
}

fn plant_legacy_rules_file(env: &TestEnv, extra: &str) {
    write(
        &rules_file(env),
        &format!("{MDC_FRONTMATTER}{LEGACY_BLOCK}\n{extra}"),
    );
}

#[test]
fn cursor_uninstall_deletes_leftover_mdc_entirely() {
    let env = TestEnv::new();
    let cursor = get_target("cursor").unwrap();
    plant_legacy_rules_file(&env, "");
    assert!(rules_file(&env).exists());

    cursor.uninstall(Location::Local);

    // The whole file — frontmatter included — is gone, not just the block.
    assert!(!rules_file(&env).exists());
}

#[test]
fn cursor_install_self_heals_leftover_mdc() {
    let env = TestEnv::new();
    let cursor = get_target("cursor").unwrap();
    plant_legacy_rules_file(&env, "");
    let result = cursor.install(Location::Local, &auto_allow());
    assert!(!rules_file(&env).exists());
    assert!(
        result
            .files
            .iter()
            .any(|f| f.path.ends_with("codegraph.mdc") && f.action == FileAction::Removed)
    );
}

#[test]
fn cursor_uninstall_preserves_user_content_outside_markers() {
    let env = TestEnv::new();
    let cursor = get_target("cursor").unwrap();
    plant_legacy_rules_file(&env, "## My own rule\nkeep me\n");

    cursor.uninstall(Location::Local);

    assert!(rules_file(&env).exists());
    let after = read(&rules_file(&env));
    assert!(after.contains("keep me"));
    // Our tool-usage block is gone.
    assert!(!after.contains("codegraph_search"));
    assert!(!after.contains("CODEGRAPH_START"));
}

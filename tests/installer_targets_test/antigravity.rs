use crate::support::*;

#[test]
fn antigravity_install_writes_legacy_path_when_no_migration_marker() {
    let env = TestEnv::new();
    let antigravity = get_target("antigravity").unwrap();
    antigravity.install(Location::Global, &auto_allow());

    let legacy_file = env
        .home()
        .join(".gemini")
        .join("antigravity")
        .join("mcp_config.json");
    assert!(legacy_file.exists());
    let cfg = read_json(&legacy_file);
    assert!(cfg["mcpServers"]["codegraph"].is_object());
    // Crucially: does NOT touch the Gemini CLI's settings.json.
    assert!(!env.home().join(".gemini").join("settings.json").exists());
}

#[test]
fn antigravity_install_writes_unified_path_when_migrated_marker_present() {
    let env = TestEnv::new();
    let antigravity = get_target("antigravity").unwrap();
    // Plant the migration marker — same signal Antigravity itself drops
    // when it migrates a user's config.
    let unified_dir = env.home().join(".gemini").join("config");
    write(&unified_dir.join(".migrated"), "");

    antigravity.install(Location::Global, &auto_allow());

    let unified_file = unified_dir.join("mcp_config.json");
    assert!(unified_file.exists());
    let cfg = read_json(&unified_file);
    assert!(cfg["mcpServers"]["codegraph"].is_object());
    // Legacy path is NOT touched when the marker tells us migration happened.
    assert!(
        !env.home()
            .join(".gemini")
            .join("antigravity")
            .join("mcp_config.json")
            .exists()
    );
}

#[test]
fn antigravity_install_writes_unified_path_when_unified_file_exists_without_marker() {
    let env = TestEnv::new();
    let antigravity = get_target("antigravity").unwrap();
    // Antigravity creates this file on first launch post-migration — its
    // presence is the second signal we accept, in case the .migrated
    // marker semantics change across Antigravity versions.
    let unified_file = env
        .home()
        .join(".gemini")
        .join("config")
        .join("mcp_config.json");
    write(&unified_file, &pretty(&json!({ "mcpServers": {} })));

    antigravity.install(Location::Global, &auto_allow());

    let cfg = read_json(&unified_file);
    assert!(cfg["mcpServers"]["codegraph"].is_object());
}

#[test]
fn antigravity_entry_has_no_type_field() {
    let env = TestEnv::new();
    let antigravity = get_target("antigravity").unwrap();
    // Marker → unified path; doesn't matter which path, just inspect the entry shape.
    write(
        &env.home().join(".gemini").join("config").join(".migrated"),
        "",
    );

    antigravity.install(Location::Global, &auto_allow());

    let cfg = read_json(
        &env.home()
            .join(".gemini")
            .join("config")
            .join("mcp_config.json"),
    );
    assert!(cfg["mcpServers"]["codegraph"].get("type").is_none());
    assert!(cfg["mcpServers"]["codegraph"]["command"].is_string());
    assert_eq!(
        cfg["mcpServers"]["codegraph"]["args"],
        json!(["serve", "--mcp"])
    );
}

#[test]
fn antigravity_install_migrates_legacy_entry_to_unified_path() {
    let env = TestEnv::new();
    let antigravity = get_target("antigravity").unwrap();
    // Simulate: user installed on the legacy path, then Antigravity
    // migrated their config (dropped the `.migrated` marker + created
    // the unified file). Re-running codegraph install should land
    // codegraph in the new file AND strip the stale legacy entry.
    let legacy_file = env
        .home()
        .join(".gemini")
        .join("antigravity")
        .join("mcp_config.json");
    write(
        &legacy_file,
        &pretty(&json!({
            "mcpServers": { "codegraph": { "command": "codegraph", "args": ["serve", "--mcp"] } }
        })),
    );
    write(
        &env.home().join(".gemini").join("config").join(".migrated"),
        "",
    );

    antigravity.install(Location::Global, &auto_allow());

    let unified = read_json(
        &env.home()
            .join(".gemini")
            .join("config")
            .join("mcp_config.json"),
    );
    assert!(unified["mcpServers"]["codegraph"].is_object());
    // Legacy file's codegraph entry got stripped.
    let legacy = read_json(&legacy_file);
    assert!(legacy.get("mcpServers").is_none());
}

#[test]
fn antigravity_install_preserves_sibling_mcp_server_legacy_path() {
    let env = TestEnv::new();
    let antigravity = get_target("antigravity").unwrap();
    let mcp_file = env
        .home()
        .join(".gemini")
        .join("antigravity")
        .join("mcp_config.json");
    write(
        &mcp_file,
        &pretty(
            &json!({ "mcpServers": { "other": { "command": "uvx", "args": ["other-server"] } } }),
        ),
    );

    antigravity.install(Location::Global, &auto_allow());

    let after = read_json(&mcp_file);
    assert!(after["mcpServers"]["other"].is_object());
    assert!(after["mcpServers"]["codegraph"].is_object());
}

#[test]
fn antigravity_install_preserves_managed_fields_on_sibling_servers() {
    let env = TestEnv::new();
    let antigravity = get_target("antigravity").unwrap();
    // Antigravity adds `"disabled": true` to entries the user disables via
    // the IDE. Install must not clobber that on sibling entries.
    write(
        &env.home().join(".gemini").join("config").join(".migrated"),
        "",
    );
    let unified = env
        .home()
        .join(".gemini")
        .join("config")
        .join("mcp_config.json");
    write(
        &unified,
        &pretty(&json!({
            "mcpServers": {
                "code-review-graph": {
                    "command": "uvx", "args": ["code-review-graph", "serve"], "disabled": true,
                },
            },
        })),
    );

    antigravity.install(Location::Global, &auto_allow());

    let after = read_json(&unified);
    assert_eq!(after["mcpServers"]["code-review-graph"]["disabled"], true);
    assert!(after["mcpServers"]["codegraph"].is_object());
}

#[test]
fn antigravity_uninstall_removes_only_codegraph_sibling_survives() {
    let env = TestEnv::new();
    let antigravity = get_target("antigravity").unwrap();
    let mcp_file = env
        .home()
        .join(".gemini")
        .join("antigravity")
        .join("mcp_config.json");
    write(
        &mcp_file,
        &pretty(
            &json!({ "mcpServers": { "other": { "command": "uvx", "args": ["other-server"] } } }),
        ),
    );

    antigravity.install(Location::Global, &auto_allow());
    antigravity.uninstall(Location::Global);

    let after = read_json(&mcp_file);
    assert!(after["mcpServers"]["other"].is_object());
    assert!(after["mcpServers"].get("codegraph").is_none());
}

#[test]
fn antigravity_uninstall_sweeps_both_legacy_and_unified_paths() {
    let env = TestEnv::new();
    let antigravity = get_target("antigravity").unwrap();
    // User had codegraph in BOTH files (e.g. legacy install + post-migration
    // re-install before our migration cleanup landed). Uninstall must clean
    // both so a "fresh slate" really is fresh.
    let legacy = env
        .home()
        .join(".gemini")
        .join("antigravity")
        .join("mcp_config.json");
    let unified = env
        .home()
        .join(".gemini")
        .join("config")
        .join("mcp_config.json");
    let entry = json!({
        "mcpServers": { "codegraph": { "command": "codegraph", "args": ["serve", "--mcp"] } }
    });
    write(&legacy, &pretty(&entry));
    write(&unified, &pretty(&entry));
    write(&unified.parent().unwrap().join(".migrated"), "");

    antigravity.uninstall(Location::Global);

    let legacy_after = read_json(&legacy);
    let unified_after = read_json(&unified);
    assert!(legacy_after.get("mcpServers").is_none());
    assert!(unified_after.get("mcpServers").is_none());
}

#[test]
fn antigravity_rejects_local_location_with_clear_note() {
    let _env = TestEnv::new();
    let antigravity = get_target("antigravity").unwrap();
    assert!(!antigravity.supports_location(Location::Local));
    let result = antigravity.install(Location::Local, &auto_allow());
    assert!(result.files.is_empty());
    assert!(result.notes.join(" ").contains("no project-local config"));
}

#[test]
fn antigravity_does_not_write_gemini_md() {
    let env = TestEnv::new();
    let antigravity = get_target("antigravity").unwrap();
    antigravity.install(Location::Global, &auto_allow());
    assert!(!env.home().join(".gemini").join("GEMINI.md").exists());
}

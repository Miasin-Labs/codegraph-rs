use crate::support::*;

// ===========================================================================
// Installer targets — contract
// ===========================================================================

#[test]
fn contract_install_writes_files_and_detect_becomes_configured() {
    for target in ALL_TARGETS {
        for location in supported_locations(target) {
            let env = TestEnv::new();
            assert!(
                !target.detect(location).already_configured,
                "{} {location}: pre-install alreadyConfigured",
                target.id()
            );

            let result = target.install(location, &auto_allow());
            assert!(
                !result.files.is_empty(),
                "{} {location}: install wrote no files",
                target.id()
            );
            for file in &result.files {
                if file.action != FileAction::Unchanged {
                    assert!(
                        file.path.exists(),
                        "{} {location}: missing {}",
                        target.id(),
                        file.path.display()
                    );
                }
            }

            assert!(
                target.detect(location).already_configured,
                "{} {location}: post-install alreadyConfigured",
                target.id()
            );
            drop(env);
        }
    }
}

#[test]
fn contract_reinstall_is_idempotent() {
    for target in ALL_TARGETS {
        for location in supported_locations(target) {
            let env = TestEnv::new();
            target.install(location, &auto_allow());
            let second = target.install(location, &auto_allow());
            for file in &second.files {
                assert_eq!(
                    file.action,
                    FileAction::Unchanged,
                    "{} {location}: {} not unchanged on re-run",
                    target.id(),
                    file.path.display()
                );
            }
            drop(env);
        }
    }
}

#[test]
fn contract_install_preserves_sibling_mcp_server() {
    for target in ALL_TARGETS {
        for location in supported_locations(target) {
            let env = TestEnv::new();
            // Plant a sibling entry in the same JSON config, install,
            // and verify the sibling survives. Skip for Codex (TOML)
            // and any target with no JSON config — they get covered
            // by their own dedicated tests below.
            let paths = target.describe_paths(location);
            // Match .json or .jsonc — opencode prefers .jsonc.
            let json_path = paths.iter().find(|p| {
                p.extension()
                    .map(|e| e == "json" || e == "jsonc")
                    .unwrap_or(false)
            });
            let json_path = match json_path {
                None => {
                    drop(env);
                    continue;
                }
                Some(p) => p.clone(),
            };

            // Seed pre-existing config. opencode uses `mcp` not `mcpServers`.
            let seed = if target.id().as_str() == "opencode" {
                json!({ "mcp": { "other": { "type": "local", "command": ["x"], "enabled": true } } })
            } else {
                json!({ "mcpServers": { "other": { "command": "x" } } })
            };
            write(&json_path, &pretty(&seed));

            target.install(location, &auto_allow());

            let after = read_json(&json_path);
            if target.id().as_str() == "opencode" {
                assert!(
                    after["mcp"]["other"].is_object(),
                    "{}: sibling lost",
                    target.id()
                );
                assert!(
                    after["mcp"]["codegraph"].is_object(),
                    "{}: codegraph missing",
                    target.id()
                );
            } else {
                assert!(
                    after["mcpServers"]["other"].is_object(),
                    "{}: sibling lost",
                    target.id()
                );
                assert!(
                    after["mcpServers"]["codegraph"].is_object(),
                    "{}: codegraph missing",
                    target.id()
                );
            }
            drop(env);
        }
    }
}

#[test]
fn contract_uninstall_reverses_install() {
    for target in ALL_TARGETS {
        for location in supported_locations(target) {
            let env = TestEnv::new();
            target.install(location, &auto_allow());
            assert!(target.detect(location).already_configured);

            target.uninstall(location);
            assert!(
                !target.detect(location).already_configured,
                "{} {location}: still configured after uninstall",
                target.id()
            );
            drop(env);
        }
    }
}

#[test]
fn contract_print_config_is_nonempty_and_writes_nothing() {
    for target in ALL_TARGETS {
        for location in supported_locations(target) {
            let env = TestEnv::new();
            let mut before: Vec<PathBuf> = list_all_files(env.home());
            before.extend(list_all_files(env.cwd()));
            let out = target.print_config(location);
            assert!(!out.is_empty(), "{}: empty printConfig", target.id());
            let mut after: Vec<PathBuf> = list_all_files(env.home());
            after.extend(list_all_files(env.cwd()));
            before.sort();
            after.sort();
            assert_eq!(after, before, "{}: printConfig touched the fs", target.id());
            drop(env);
        }
    }
}

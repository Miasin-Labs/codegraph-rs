use crate::support::*;

// ---- Legacy auto-sync hook cleanup ----
// Pre-0.8 installs wrote `codegraph mark-dirty` / `sync-if-dirty`
// hooks to settings.json. Both subcommands were removed from the CLI,
// so the Stop hook fails every turn ("unknown command
// 'sync-if-dirty'"). The installer must strip them on upgrade and
// uninstall — without touching the user's unrelated hooks.

fn seed_settings(env: &TestEnv, loc: Location, settings: &Value) -> PathBuf {
    let dir = match loc {
        Location::Global => env.home().join(".claude"),
        Location::Local => env.cwd().join(".claude"),
    };
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("settings.json");
    fs::write(&file, pretty(settings)).unwrap();
    file
}

// Realistic pre-0.8 settings.json: our two auto-sync hooks plus an
// unrelated GitKraken Stop hook the user added (matches the report).
fn legacy_hook_settings() -> Value {
    json!({
        "hooks": {
            "PostToolUse": [
                { "matcher": "Edit|Write", "hooks": [{ "type": "command", "command": "codegraph mark-dirty", "async": true }] },
            ],
            "Stop": [
                { "hooks": [{ "type": "command", "command": "codegraph sync-if-dirty" }] },
                { "hooks": [{ "type": "command", "command": "\"/Users/me/gk\" ai hook run --host claude-code" }] },
            ],
        },
    })
}

#[test]
fn claude_install_strips_stale_hooks_but_keeps_gitkraken_hook() {
    let env = TestEnv::new();
    let claude = get_target("claude").unwrap();
    let file = seed_settings(&env, Location::Global, &legacy_hook_settings());

    claude.install(Location::Global, &auto_allow());

    let after = read_json(&file);
    // The only PostToolUse group held mark-dirty → the event is gone.
    assert!(after["hooks"].get("PostToolUse").is_none());
    let stop_commands: Vec<String> = after["hooks"]["Stop"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .flat_map(|g| {
            g["hooks"]
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .filter_map(|h| h["command"].as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(!stop_commands.iter().any(|c| c == "codegraph sync-if-dirty"));
    // The unrelated GitKraken hook survives untouched.
    assert!(
        stop_commands
            .iter()
            .any(|c| c.contains("gk") && c.contains("ai hook run"))
    );
    // Permissions still written as normal alongside the cleanup.
    assert!(
        after["permissions"]["allow"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p == "mcp__codegraph__codegraph_search")
    );
}

#[test]
fn claude_cleanup_legacy_hooks_preserves_sibling_hook_sharing_matcher_group() {
    let env = TestEnv::new();
    let file = seed_settings(
        &env,
        Location::Global,
        &json!({
            "hooks": {
                "Stop": [
                    {
                        "hooks": [
                            { "type": "command", "command": "codegraph sync-if-dirty" },
                            { "type": "command", "command": "gk ai hook run --host claude-code" },
                        ],
                    },
                ],
            },
        }),
    );

    assert_eq!(
        cleanup_legacy_hooks(Location::Global).action,
        FileAction::Removed
    );

    let after = read_json(&file);
    let commands: Vec<&str> = after["hooks"]["Stop"][0]["hooks"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|h| h["command"].as_str())
        .collect();
    assert_eq!(commands, vec!["gk ai hook run --host claude-code"]);
}

#[test]
fn claude_cleanup_legacy_hooks_is_byte_for_byte_noop_without_codegraph_hooks() {
    let env = TestEnv::new();
    let settings = json!({
        "hooks": { "Stop": [{ "hooks": [{ "type": "command", "command": "gk ai hook run" }] }] }
    });
    let file = seed_settings(&env, Location::Global, &settings);
    let original = read(&file);

    assert_eq!(
        cleanup_legacy_hooks(Location::Global).action,
        FileAction::Unchanged
    );
    assert_eq!(read(&file), original);
}

#[test]
fn claude_cleanup_legacy_hooks_reports_not_found_when_settings_absent() {
    let _env = TestEnv::new();
    assert_eq!(
        cleanup_legacy_hooks(Location::Global).action,
        FileAction::NotFound
    );
}

#[test]
fn claude_rerunning_install_after_legacy_cleanup_leaves_settings_unchanged() {
    let env = TestEnv::new();
    let claude = get_target("claude").unwrap();
    let file = seed_settings(&env, Location::Global, &legacy_hook_settings());
    claude.install(Location::Global, &auto_allow());
    let first_pass = read(&file);
    claude.install(Location::Global, &auto_allow());
    assert_eq!(read(&file), first_pass);
}

#[test]
fn claude_uninstall_strips_stale_hooks_written_in_npx_form_local() {
    let env = TestEnv::new();
    let claude = get_target("claude").unwrap();
    let file = seed_settings(
        &env,
        Location::Local,
        &json!({
            "hooks": {
                "PostToolUse": [
                    { "matcher": "Edit|Write", "hooks": [{ "type": "command", "command": "npx @colbymchenry/codegraph mark-dirty", "async": true }] },
                ],
                "Stop": [
                    { "hooks": [{ "type": "command", "command": "npx @colbymchenry/codegraph sync-if-dirty" }] },
                ],
            },
        }),
    );

    claude.uninstall(Location::Local);

    let after = read_json(&file);
    // Both events emptied → the whole `hooks` object is removed.
    assert!(after.get("hooks").is_none());
}

// ---- UserPromptSubmit front-load hook ----

#[test]
fn claude_install_adds_prompt_hook_and_preserves_sibling_settings() {
    let env = TestEnv::new();
    let claude = get_target("claude").unwrap();
    let file = seed_settings(
        &env,
        Location::Local,
        &json!({
            "theme": "dark",
            "hooks": {
                "UserPromptSubmit": [
                    { "hooks": [{ "type": "command", "command": "my prompt hook" }] },
                ],
                "Stop": [
                    { "hooks": [{ "type": "command", "command": "my stop hook" }] },
                ],
            },
        }),
    );

    claude.install(Location::Local, &prompt_hook(true));

    let after = read_json(&file);
    assert_eq!(after["theme"], "dark");
    assert_eq!(
        after["hooks"]["UserPromptSubmit"].as_array().unwrap().len(),
        2
    );
    let commands: Vec<&str> = after["hooks"]["UserPromptSubmit"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|group| group["hooks"].as_array().into_iter().flatten())
        .filter_map(|hook| hook["command"].as_str())
        .collect();
    assert!(commands.contains(&"my prompt hook"));
    assert!(commands.contains(&"codegraph prompt-hook"));
    assert_eq!(
        after["hooks"]["Stop"][0]["hooks"][0]["command"],
        "my stop hook"
    );
}

#[test]
fn claude_prompt_hook_install_is_byte_for_byte_idempotent() {
    let env = TestEnv::new();
    let claude = get_target("claude").unwrap();
    let settings = env.cwd().join(".claude").join("settings.json");

    claude.install(Location::Local, &prompt_hook(true));
    let first = read(&settings);
    let second = claude.install(Location::Local, &prompt_hook(true));

    assert_eq!(read(&settings), first);
    assert_eq!(
        second
            .files
            .iter()
            .find(|file| file.path.ends_with("settings.json"))
            .map(|file| file.action),
        Some(FileAction::Unchanged)
    );
}

#[test]
fn claude_prompt_hook_opt_out_removes_only_codegraph_command() {
    let env = TestEnv::new();
    let claude = get_target("claude").unwrap();
    let file = seed_settings(
        &env,
        Location::Global,
        &json!({
            "theme": "dark",
            "hooks": {
                "UserPromptSubmit": [
                    {
                        "hooks": [
                            { "type": "command", "command": "npx @colbymchenry/codegraph prompt-hook" },
                            { "type": "command", "command": "my prompt hook" },
                        ],
                    },
                ],
            },
        }),
    );

    claude.install(Location::Global, &prompt_hook(false));

    let after = read_json(&file);
    assert_eq!(after["theme"], "dark");
    let commands: Vec<&str> = after["hooks"]["UserPromptSubmit"][0]["hooks"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|hook| hook["command"].as_str())
        .collect();
    assert_eq!(commands, vec!["my prompt hook"]);
}

#[test]
fn claude_uninstall_removes_prompt_hook_but_keeps_user_hooks() {
    let env = TestEnv::new();
    let claude = get_target("claude").unwrap();
    let file = seed_settings(
        &env,
        Location::Local,
        &json!({
            "hooks": {
                "UserPromptSubmit": [
                    { "hooks": [{ "type": "command", "command": "codegraph prompt-hook" }] },
                    { "hooks": [{ "type": "command", "command": "my prompt hook" }] },
                ],
            },
        }),
    );

    claude.uninstall(Location::Local);

    let after = read_json(&file);
    assert_eq!(
        after["hooks"]["UserPromptSubmit"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        after["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"],
        "my prompt hook"
    );
}

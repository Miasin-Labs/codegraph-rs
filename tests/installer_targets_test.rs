//! Multi-target installer tests (port of `__tests__/installer-targets.test.ts`
//! plus `__tests__/installer.test.ts`).
//!
//! Each `AgentTarget` is exercised against the same contract:
//!   - `install` writes the expected files
//!   - re-running `install` is byte-identical (idempotent)
//!   - sibling MCP servers / unrelated config is preserved
//!   - `uninstall` reverses `install`
//!   - `print_config` returns parseable, non-empty content
//!
//! For agent-config destinations we redirect HOME to a tmpdir via the
//! `$HOME` (POSIX) / `%USERPROFILE%` (Windows) env vars, and CWD via
//! `std::env::set_current_dir` — same pattern as the TS suite. No real
//! `~/.claude/` etc. is ever touched.
//!
//! Env + cwd are process-global, so every test acquires a shared mutex
//! (the TS suite got the same serialization from vitest's
//! single-threaded per-file execution).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::{env, fs};

use codegraph::installer::UninstallStatus;
use codegraph::installer::config_writer::write_mcp_config;
use codegraph::installer::install::uninstall_targets;
use codegraph::installer::targets::claude::cleanup_legacy_hooks;
use codegraph::installer::targets::registry::{ALL_TARGETS, get_target, resolve_target_flag};
use codegraph::installer::targets::toml::{
    TomlRemoveAction,
    TomlUpsertAction,
    TomlValue,
    build_toml_table,
    remove_toml_table,
    upsert_toml_table,
};
use codegraph::installer::targets::types::{AgentTarget, FileAction, InstallOptions, Location};
use serde_json::{Value, json};

static ENV_MUTEX: Mutex<()> = Mutex::new(());

const SAVED_VARS: [&str; 5] = [
    "HOME",
    "USERPROFILE",
    "APPDATA",
    "XDG_CONFIG_HOME",
    "HERMES_HOME",
];

/// RAII guard that redirects HOME + cwd into temp dirs and restores on drop.
struct TestEnv {
    _guard: MutexGuard<'static, ()>,
    home: PathBuf,
    cwd: PathBuf,
    orig_cwd: PathBuf,
    saved: Vec<(&'static str, Option<OsString>)>,
    _home_dir: tempfile::TempDir,
    _cwd_dir: tempfile::TempDir,
}

impl TestEnv {
    fn new() -> Self {
        let guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home_dir = tempfile::Builder::new()
            .prefix("cg-targets-home-")
            .tempdir()
            .unwrap();
        let cwd_dir = tempfile::Builder::new()
            .prefix("cg-targets-cwd-")
            .tempdir()
            .unwrap();
        // Canonicalize so paths derived from `env::current_dir()` (which
        // resolves symlinks like macOS's /var → /private/var) compare
        // equal to paths we build from these roots.
        let home = home_dir.path().canonicalize().unwrap();
        let cwd = cwd_dir.path().canonicalize().unwrap();
        let orig_cwd = env::current_dir().unwrap();

        let saved: Vec<(&'static str, Option<OsString>)> =
            SAVED_VARS.iter().map(|&k| (k, env::var_os(k))).collect();

        env::set_var("HOME", &home);
        env::set_var("USERPROFILE", &home);
        env::set_var("APPDATA", home.join(".config"));
        env::set_var("XDG_CONFIG_HOME", home.join(".config"));
        env::remove_var("HERMES_HOME");
        env::set_current_dir(&cwd).unwrap();

        TestEnv {
            _guard: guard,
            home,
            cwd,
            orig_cwd,
            saved,
            _home_dir: home_dir,
            _cwd_dir: cwd_dir,
        }
    }

    fn home(&self) -> &Path {
        &self.home
    }

    fn cwd(&self) -> &Path {
        &self.cwd
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        let _ = env::set_current_dir(&self.orig_cwd);
        for (k, v) in &self.saved {
            match v {
                Some(val) => env::set_var(k, val),
                None => env::remove_var(k),
            }
        }
    }
}

fn write(path: &Path, content: &str) {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&read(path)).unwrap()
}

fn pretty(value: &Value) -> String {
    format!("{}\n", serde_json::to_string_pretty(value).unwrap())
}

fn auto_allow() -> InstallOptions {
    InstallOptions { auto_allow: true }
}

fn no_allow() -> InstallOptions {
    InstallOptions { auto_allow: false }
}

/// A marker-delimited CodeGraph block exactly as a previous installer
/// wrote it. Issue #529: the installer no longer writes an instructions
/// file, but install (self-heal on upgrade) and uninstall both still
/// strip a block a prior install left, so we plant this to exercise it.
const LEGACY_BLOCK: &str = "<!-- CODEGRAPH_START -->\n## CodeGraph\n\nPrefer `codegraph_search` / `codegraph_callers` over grep.\n<!-- CODEGRAPH_END -->";

fn list_all_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !dir.exists() {
        return out;
    }
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let full = entry.path();
        if entry.file_type().unwrap().is_dir() {
            out.extend(list_all_files(&full));
        } else {
            out.push(full);
        }
    }
    out
}

fn supported_locations(target: &dyn AgentTarget) -> Vec<Location> {
    [Location::Global, Location::Local]
        .into_iter()
        .filter(|l| target.supports_location(*l))
        .collect()
}

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

// ===========================================================================
// Installer targets — partial-state idempotency
// ===========================================================================

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
fn gemini_install_writes_settings_json_and_no_gemini_md() {
    let env = TestEnv::new();
    let gemini = get_target("gemini").unwrap();
    let result = gemini.install(Location::Global, &auto_allow());
    let settings = env.home().join(".gemini").join("settings.json");
    let gemini_md = env.home().join(".gemini").join("GEMINI.md");
    assert!(result.files.iter().any(|f| f.path == settings));
    assert!(!result.files.iter().any(|f| f.path == gemini_md));
    assert!(!gemini_md.exists());

    let cfg = read_json(&settings);
    assert_eq!(
        cfg["mcpServers"]["codegraph"],
        json!({ "type": "stdio", "command": "codegraph", "args": ["serve", "--mcp"] })
    );
}

#[test]
fn gemini_install_preserves_preexisting_settings() {
    let env = TestEnv::new();
    let gemini = get_target("gemini").unwrap();
    let settings = env.home().join(".gemini").join("settings.json");
    write(
        &settings,
        &pretty(&json!({ "security": { "auth": { "selectedType": "oauth-personal" } } })),
    );

    gemini.install(Location::Global, &auto_allow());

    let after = read_json(&settings);
    assert_eq!(after["security"]["auth"]["selectedType"], "oauth-personal");
    assert!(after["mcpServers"]["codegraph"].is_object());
}

#[test]
fn gemini_uninstall_strips_codegraph_but_leaves_preexisting_settings() {
    let env = TestEnv::new();
    let gemini = get_target("gemini").unwrap();
    let settings = env.home().join(".gemini").join("settings.json");
    write(
        &settings,
        &pretty(&json!({ "security": { "auth": { "selectedType": "oauth-personal" } } })),
    );

    gemini.install(Location::Global, &auto_allow());
    gemini.uninstall(Location::Global);

    let after = read_json(&settings);
    assert_eq!(after["security"]["auth"]["selectedType"], "oauth-personal");
    assert!(after.get("mcpServers").is_none());
}

#[test]
fn gemini_local_install_writes_local_settings_and_never_gemini_md() {
    let env = TestEnv::new();
    let gemini = get_target("gemini").unwrap();
    let result = gemini.install(Location::Local, &auto_allow());
    let paths: Vec<String> = result
        .files
        .iter()
        .map(|f| f.path.to_string_lossy().replace('\\', "/"))
        .collect();
    assert!(paths.iter().any(|p| p.ends_with("/.gemini/settings.json")));
    assert!(!paths.iter().any(|p| p.ends_with("/GEMINI.md")));
    assert!(!env.cwd().join("GEMINI.md").exists());
}

#[test]
fn gemini_uninstall_strips_leftover_gemini_md_block_keeping_user_content() {
    let env = TestEnv::new();
    let gemini = get_target("gemini").unwrap();
    let gemini_md = env.home().join(".gemini").join("GEMINI.md");
    write(
        &gemini_md,
        &format!("# My personal Gemini context\n\nAlways respond concisely.\n\n{LEGACY_BLOCK}\n"),
    );

    gemini.uninstall(Location::Global);

    let body = read(&gemini_md);
    assert!(body.contains("# My personal Gemini context"));
    assert!(body.contains("Always respond concisely."));
    assert!(!body.contains("CODEGRAPH_START"));
}

#[test]
fn kiro_install_writes_mcp_json_and_no_steering_doc() {
    let env = TestEnv::new();
    let kiro = get_target("kiro").unwrap();
    let result = kiro.install(Location::Global, &auto_allow());
    let mcp = env.home().join(".kiro").join("settings").join("mcp.json");
    let steering = env
        .home()
        .join(".kiro")
        .join("steering")
        .join("codegraph.md");
    assert!(result.files.iter().any(|f| f.path == mcp));
    assert!(!result.files.iter().any(|f| f.path == steering));
    assert!(!steering.exists());

    let cfg = read_json(&mcp);
    assert_eq!(
        cfg["mcpServers"]["codegraph"],
        json!({ "type": "stdio", "command": "codegraph", "args": ["serve", "--mcp"] })
    );
}

#[test]
fn kiro_install_deletes_leftover_steering_doc_self_heal() {
    let env = TestEnv::new();
    let kiro = get_target("kiro").unwrap();
    let steering = env
        .home()
        .join(".kiro")
        .join("steering")
        .join("codegraph.md");
    write(&steering, &format!("{LEGACY_BLOCK}\n"));

    let result = kiro.install(Location::Global, &auto_allow());
    assert!(!steering.exists());
    assert_eq!(
        result
            .files
            .iter()
            .find(|f| f.path == steering)
            .map(|f| f.action),
        Some(FileAction::Removed)
    );
}

#[test]
fn kiro_install_preserves_sibling_mcp_server() {
    let env = TestEnv::new();
    let kiro = get_target("kiro").unwrap();
    let mcp = env.home().join(".kiro").join("settings").join("mcp.json");
    write(
        &mcp,
        &pretty(
            &json!({ "mcpServers": { "other": { "command": "uvx", "args": ["other-server"] } } }),
        ),
    );

    kiro.install(Location::Global, &auto_allow());

    let after = read_json(&mcp);
    assert!(after["mcpServers"]["other"].is_object());
    assert!(after["mcpServers"]["codegraph"].is_object());
}

#[test]
fn kiro_uninstall_strips_codegraph_but_leaves_siblings() {
    let env = TestEnv::new();
    let kiro = get_target("kiro").unwrap();
    let mcp = env.home().join(".kiro").join("settings").join("mcp.json");
    write(
        &mcp,
        &pretty(
            &json!({ "mcpServers": { "other": { "command": "uvx", "args": ["other-server"] } } }),
        ),
    );

    kiro.install(Location::Global, &auto_allow());
    kiro.uninstall(Location::Global);

    let after = read_json(&mcp);
    assert!(after["mcpServers"]["other"].is_object());
    assert!(after["mcpServers"].get("codegraph").is_none());
}

#[test]
fn kiro_uninstall_removes_leftover_steering_doc_outright() {
    let env = TestEnv::new();
    let kiro = get_target("kiro").unwrap();
    let steering = env
        .home()
        .join(".kiro")
        .join("steering")
        .join("codegraph.md");
    write(&steering, &format!("{LEGACY_BLOCK}\n"));

    kiro.uninstall(Location::Global);
    assert!(!steering.exists());
}

#[test]
fn kiro_uninstall_leaves_sibling_steering_doc_untouched() {
    let env = TestEnv::new();
    let kiro = get_target("kiro").unwrap();
    let sibling = env.home().join(".kiro").join("steering").join("product.md");
    let ours = env
        .home()
        .join(".kiro")
        .join("steering")
        .join("codegraph.md");
    write(&sibling, "# Product\n\nMy team practices.\n");
    write(&ours, &format!("{LEGACY_BLOCK}\n"));

    kiro.uninstall(Location::Global);

    assert!(!ours.exists());
    assert!(sibling.exists());
    assert!(read(&sibling).contains("My team practices."));
}

#[test]
fn kiro_local_install_writes_local_mcp_json_and_no_steering_doc() {
    let _env = TestEnv::new();
    let kiro = get_target("kiro").unwrap();
    let result = kiro.install(Location::Local, &auto_allow());
    let paths: Vec<String> = result
        .files
        .iter()
        .map(|f| f.path.to_string_lossy().replace('\\', "/"))
        .collect();
    assert!(
        paths
            .iter()
            .any(|p| p.ends_with("/.kiro/settings/mcp.json"))
    );
    assert!(
        !paths
            .iter()
            .any(|p| p.ends_with("/.kiro/steering/codegraph.md"))
    );
}

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

#[test]
fn gemini_and_antigravity_coexist() {
    let env = TestEnv::new();
    let gemini = get_target("gemini").unwrap();
    let antigravity = get_target("antigravity").unwrap();
    gemini.install(Location::Global, &auto_allow());
    antigravity.install(Location::Global, &auto_allow());

    let cli_cfg = read_json(&env.home().join(".gemini").join("settings.json"));
    // Antigravity lands on the LEGACY path here since no .migrated marker
    // was planted — same end-to-end check either way.
    let ide_cfg = read_json(
        &env.home()
            .join(".gemini")
            .join("antigravity")
            .join("mcp_config.json"),
    );
    assert!(cli_cfg["mcpServers"]["codegraph"].is_object());
    assert!(ide_cfg["mcpServers"]["codegraph"].is_object());

    // Uninstall one — the other's MCP entry must survive.
    antigravity.uninstall(Location::Global);
    let cli_after = read_json(&env.home().join(".gemini").join("settings.json"));
    assert!(cli_after["mcpServers"]["codegraph"].is_object());
}

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

#[test]
fn claude_local_install_writes_mcp_json_not_claude_json() {
    let env = TestEnv::new();
    let claude = get_target("claude").unwrap();
    let result = claude.install(Location::Local, &no_allow());
    // The MCP entry lands in ./.mcp.json — the file Claude Code reads.
    assert!(result.files.iter().any(|f| {
        f.path
            .to_string_lossy()
            .replace('\\', "/")
            .ends_with("/.mcp.json")
    }));
    assert!(env.cwd().join(".mcp.json").exists());
    assert!(!env.cwd().join(".claude.json").exists());
    let cfg = read_json(&env.cwd().join(".mcp.json"));
    assert!(cfg["mcpServers"]["codegraph"].is_object());
}

#[test]
fn claude_install_does_not_create_claude_md() {
    let env = TestEnv::new();
    let claude = get_target("claude").unwrap();
    let result = claude.install(Location::Local, &no_allow());
    let claude_md = env.cwd().join(".claude").join("CLAUDE.md");
    assert!(!claude_md.exists());
    assert!(!result.files.iter().any(|f| f.path.ends_with("CLAUDE.md")));
}

#[test]
fn claude_install_strips_legacy_claude_md_block_keeping_user_content() {
    let env = TestEnv::new();
    let claude = get_target("claude").unwrap();
    let claude_md = env.cwd().join(".claude").join("CLAUDE.md");
    write(
        &claude_md,
        &format!("# My project rules\n\nUse tabs.\n\n{LEGACY_BLOCK}\n"),
    );

    let result = claude.install(Location::Local, &no_allow());

    let body = read(&claude_md);
    assert!(body.contains("# My project rules"));
    assert!(body.contains("Use tabs."));
    assert!(!body.contains("CODEGRAPH_START"));
    assert_eq!(
        result
            .files
            .iter()
            .find(|f| f.path.ends_with("CLAUDE.md"))
            .map(|f| f.action),
        Some(FileAction::Removed)
    );
}

#[test]
fn claude_global_install_targets_home_claude_json() {
    let env = TestEnv::new();
    let claude = get_target("claude").unwrap();
    claude.install(Location::Global, &no_allow());
    let cfg = read_json(&env.home().join(".claude.json"));
    assert!(cfg["mcpServers"]["codegraph"].is_object());
}

#[test]
fn claude_local_install_migrates_legacy_claude_json_entry() {
    let env = TestEnv::new();
    let claude = get_target("claude").unwrap();
    let legacy = env.cwd().join(".claude.json");
    write(
        &legacy,
        &serde_json::to_string_pretty(&json!({
            "mcpServers": { "codegraph": { "type": "stdio", "command": "codegraph", "args": ["serve", "--mcp"] } }
        }))
        .unwrap(),
    );

    claude.install(Location::Local, &no_allow());

    // codegraph now lives in .mcp.json; the legacy file (which held only
    // codegraph) is gone.
    let mcp = read_json(&env.cwd().join(".mcp.json"));
    assert!(mcp["mcpServers"]["codegraph"].is_object());
    assert!(!legacy.exists());
}

#[test]
fn claude_legacy_migration_preserves_sibling_servers_and_unrelated_keys() {
    let env = TestEnv::new();
    let claude = get_target("claude").unwrap();
    let legacy = env.cwd().join(".claude.json");
    write(
        &legacy,
        &serde_json::to_string_pretty(&json!({
            "mcpServers": {
                "codegraph": { "type": "stdio", "command": "codegraph", "args": ["serve", "--mcp"] },
                "other": { "command": "x" },
            },
            "somethingElse": true,
        }))
        .unwrap(),
    );

    claude.install(Location::Local, &no_allow());

    // Only codegraph is stripped from the legacy file; siblings survive.
    let after = read_json(&legacy);
    assert!(after["mcpServers"].get("codegraph").is_none());
    assert!(after["mcpServers"]["other"].is_object());
    assert_eq!(after["somethingElse"], true);
    let mcp = read_json(&env.cwd().join(".mcp.json"));
    assert!(mcp["mcpServers"]["codegraph"].is_object());
}

#[test]
fn claude_uninstall_strips_codegraph_from_mcp_json_and_legacy_claude_json() {
    let env = TestEnv::new();
    let claude = get_target("claude").unwrap();
    // A user left with both the working .mcp.json and a stale .claude.json.
    write(
        &env.cwd().join(".mcp.json"),
        &serde_json::to_string_pretty(
            &json!({ "mcpServers": { "codegraph": { "command": "codegraph" } } }),
        )
        .unwrap(),
    );
    write(
        &env.cwd().join(".claude.json"),
        &serde_json::to_string_pretty(&json!({
            "mcpServers": { "codegraph": { "command": "codegraph" }, "other": { "command": "x" } }
        }))
        .unwrap(),
    );

    claude.uninstall(Location::Local);

    let mcp = read_json(&env.cwd().join(".mcp.json"));
    assert!(mcp.get("mcpServers").is_none());
    let legacy = read_json(&env.cwd().join(".claude.json"));
    assert!(legacy["mcpServers"].get("codegraph").is_none());
    assert!(legacy["mcpServers"]["other"].is_object());
}

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

// ===========================================================================
// Installer targets — registry
// ===========================================================================

#[test]
fn registry_get_target_returns_right_target_for_each_id() {
    for id in [
        "claude",
        "cursor",
        "codex",
        "opencode",
        "hermes",
        "gemini",
        "antigravity",
        "kiro",
    ] {
        assert_eq!(get_target(id).map(|t| t.id().as_str()), Some(id));
    }
    assert!(get_target("not-a-real-target").is_none());
}

#[test]
fn registry_resolve_target_flag_handles_all_none_csv() {
    // (the `auto` arm probes the real environment; covered implicitly
    // by the fallback test below)
    let _env = TestEnv::new();
    assert!(
        resolve_target_flag("none", Location::Global)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        resolve_target_flag("all", Location::Global).unwrap().len(),
        ALL_TARGETS.len()
    );
    let csv = resolve_target_flag("claude,cursor", Location::Global).unwrap();
    let ids: Vec<&str> = csv.iter().map(|t| t.id().as_str()).collect();
    assert_eq!(ids, vec!["claude", "cursor"]);
}

#[test]
fn registry_resolve_target_flag_errors_on_unknown_id() {
    let err = match resolve_target_flag("claude,bogus", Location::Global) {
        Ok(_) => panic!("expected an error for an unknown --target id"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("Unknown --target"));
}

// ===========================================================================
// Installer targets — TOML serializer (Codex backbone)
// (Also covered as unit tests in src/installer/targets/toml.rs.)
// ===========================================================================

#[test]
fn toml_builds_codegraph_block() {
    let block = build_toml_table(
        "mcp_servers.codegraph",
        &[
            ("command", TomlValue::String("codegraph".to_string())),
            (
                "args",
                TomlValue::Array(vec!["serve".to_string(), "--mcp".to_string()]),
            ),
        ],
    );
    assert!(block.contains("[mcp_servers.codegraph]"));
    assert!(block.contains("command = \"codegraph\""));
    assert!(block.contains("args = [\"serve\", \"--mcp\"]"));
}

#[test]
fn toml_upsert_inserts_into_empty_content() {
    let block = build_toml_table(
        "mcp_servers.codegraph",
        &[
            ("command", TomlValue::String("codegraph".to_string())),
            ("args", TomlValue::Array(vec!["serve".to_string()])),
        ],
    );
    let (content, action) = upsert_toml_table("", "mcp_servers.codegraph", &block);
    assert_eq!(action, TomlUpsertAction::Inserted);
    assert!(content.starts_with("[mcp_servers.codegraph]"));
}

#[test]
fn toml_upsert_is_idempotent() {
    let block = build_toml_table(
        "mcp_servers.codegraph",
        &[
            ("command", TomlValue::String("codegraph".to_string())),
            ("args", TomlValue::Array(vec!["serve".to_string()])),
        ],
    );
    let (first, _) = upsert_toml_table("", "mcp_servers.codegraph", &block);
    let (second, action) = upsert_toml_table(&first, "mcp_servers.codegraph", &block);
    assert_eq!(action, TomlUpsertAction::Unchanged);
    assert_eq!(second, first);
}

#[test]
fn toml_upsert_replaces_existing_block_preserving_siblings() {
    let existing = [
        "[other_table]",
        "foo = \"bar\"",
        "",
        "[mcp_servers.codegraph]",
        "command = \"old-codegraph\"",
        "args = [\"old\"]",
        "",
        "[zzz]",
        "baz = \"qux\"",
        "",
    ]
    .join("\n");
    let new_block = build_toml_table(
        "mcp_servers.codegraph",
        &[
            ("command", TomlValue::String("codegraph".to_string())),
            (
                "args",
                TomlValue::Array(vec!["serve".to_string(), "--mcp".to_string()]),
            ),
        ],
    );
    let (content, action) = upsert_toml_table(&existing, "mcp_servers.codegraph", &new_block);
    assert_eq!(action, TomlUpsertAction::Replaced);
    assert!(content.contains("[other_table]"));
    assert!(content.contains("foo = \"bar\""));
    assert!(content.contains("[zzz]"));
    assert!(content.contains("baz = \"qux\""));
    assert!(content.contains("command = \"codegraph\""));
    assert!(!content.contains("old-codegraph"));
}

#[test]
fn toml_remove_strips_block_preserving_siblings() {
    let existing = [
        "[other_table]",
        "foo = \"bar\"",
        "",
        "[mcp_servers.codegraph]",
        "command = \"codegraph\"",
        "args = [\"serve\"]",
    ]
    .join("\n");
    let (content, action) = remove_toml_table(&existing, "mcp_servers.codegraph");
    assert_eq!(action, TomlRemoveAction::Removed);
    assert!(content.contains("[other_table]"));
    assert!(content.contains("foo = \"bar\""));
    assert!(!content.contains("mcp_servers.codegraph"));
}

#[test]
fn toml_remove_missing_table_returns_not_found() {
    let existing = "[other]\nfoo = \"bar\"\n";
    let (content, action) = remove_toml_table(existing, "mcp_servers.codegraph");
    assert_eq!(action, TomlRemoveAction::NotFound);
    assert_eq!(content, existing);
}

#[test]
fn toml_upsert_preserves_array_of_tables_sibling() {
    let existing = ["[[foo]]", "name = \"a\"", "", "[[foo]]", "name = \"b\"", ""].join("\n");
    let block = build_toml_table(
        "mcp_servers.codegraph",
        &[
            ("command", TomlValue::String("codegraph".to_string())),
            ("args", TomlValue::Array(vec!["serve".to_string()])),
        ],
    );
    let (content, _) = upsert_toml_table(&existing, "mcp_servers.codegraph", &block);
    assert_eq!(content.matches("[[foo]]").count(), 2);
    assert!(content.contains("[mcp_servers.codegraph]"));
}

// ===========================================================================
// Installer — uninstall_targets sweep (codegraph uninstall)
// ===========================================================================

#[test]
fn sweep_removes_every_installed_agent_global() {
    let _env = TestEnv::new();
    for t in ALL_TARGETS {
        if t.supports_location(Location::Global) {
            t.install(Location::Global, &auto_allow());
        }
    }

    let reports = uninstall_targets(&ALL_TARGETS, Location::Global);

    for t in ALL_TARGETS {
        let r = reports.iter().find(|x| x.id == t.id()).unwrap();
        assert_eq!(r.status, UninstallStatus::Removed, "{}", t.id());
        assert!(!r.removed_paths.is_empty(), "{}", t.id());
        // The actual config is gone afterward.
        assert!(!t.detect(Location::Global).already_configured, "{}", t.id());
    }
}

#[test]
fn sweep_is_safe_on_clean_slate() {
    let _env = TestEnv::new();
    let reports = uninstall_targets(&ALL_TARGETS, Location::Global);
    for r in &reports {
        assert_eq!(r.status, UninstallStatus::NotConfigured, "{}", r.id);
        assert!(r.removed_paths.is_empty());
    }
}

#[test]
fn sweep_reports_removed_only_for_configured_agents() {
    let _env = TestEnv::new();
    // Install on Claude only; the rest stay untouched.
    get_target("claude")
        .unwrap()
        .install(Location::Global, &auto_allow());

    let reports = uninstall_targets(&ALL_TARGETS, Location::Global);

    let claude = reports.iter().find(|r| r.id.as_str() == "claude").unwrap();
    assert_eq!(claude.status, UninstallStatus::Removed);
    assert_eq!(
        claude.display_name,
        get_target("claude").unwrap().display_name()
    );

    for r in reports.iter().filter(|x| x.id.as_str() != "claude") {
        assert_eq!(r.status, UninstallStatus::NotConfigured, "{}", r.id);
    }
}

#[test]
fn sweep_marks_global_only_agents_unsupported_for_local() {
    let _env = TestEnv::new();
    let reports = uninstall_targets(&ALL_TARGETS, Location::Local);
    for t in ALL_TARGETS {
        let r = reports.iter().find(|x| x.id == t.id()).unwrap();
        if t.supports_location(Location::Local) {
            assert_eq!(r.status, UninstallStatus::NotConfigured, "{}", t.id());
        } else {
            assert_eq!(r.status, UninstallStatus::Unsupported, "{}", t.id());
            assert!(r.removed_paths.is_empty());
            assert!(r.notes[0].contains("global-only"));
        }
    }
}

#[test]
fn sweep_is_idempotent() {
    let _env = TestEnv::new();
    for t in ALL_TARGETS {
        if t.supports_location(Location::Global) {
            t.install(Location::Global, &auto_allow());
        }
    }
    let first = uninstall_targets(&ALL_TARGETS, Location::Global);
    assert!(first.iter().any(|r| r.status == UninstallStatus::Removed));

    let second = uninstall_targets(&ALL_TARGETS, Location::Global);
    for r in &second {
        assert_eq!(r.status, UninstallStatus::NotConfigured, "{}", r.id);
        assert!(r.removed_paths.is_empty());
    }
}

#[test]
fn sweep_target_subset_removes_only_chosen_agents() {
    let _env = TestEnv::new();
    get_target("claude")
        .unwrap()
        .install(Location::Global, &auto_allow());
    get_target("cursor")
        .unwrap()
        .install(Location::Global, &auto_allow());

    let subset = resolve_target_flag("claude", Location::Global).unwrap();
    let reports = uninstall_targets(&subset, Location::Global);

    let ids: Vec<&str> = reports.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["claude"]);
    assert_eq!(reports[0].status, UninstallStatus::Removed);
    // Cursor was not in the subset — still configured.
    assert!(
        get_target("cursor")
            .unwrap()
            .detect(Location::Global)
            .already_configured
    );
    assert!(
        !get_target("claude")
            .unwrap()
            .detect(Location::Global)
            .already_configured
    );
}

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

// ===========================================================================
// Installer Config Writer (port of __tests__/installer.test.ts)
// ===========================================================================

#[test]
fn config_writer_creates_mcp_json_when_missing() {
    let env = TestEnv::new();
    // write_mcp_config reads .mcp.json - if it doesn't exist, it should create it
    write_mcp_config(Location::Local);

    let mcp_json = env.cwd().join(".mcp.json");
    assert!(mcp_json.exists());

    let content = read_json(&mcp_json);
    assert!(content["mcpServers"].is_object());
    assert!(content["mcpServers"]["codegraph"].is_object());
}

#[test]
fn config_writer_handles_corrupted_json_by_creating_backup() {
    let env = TestEnv::new();
    // Create a corrupted .mcp.json
    let mcp_json = env.cwd().join(".mcp.json");
    fs::write(&mcp_json, "{ this is not valid json !!!").unwrap();

    // Should not panic - gracefully handles corruption.
    // (TS asserts a console.warn spy fired; the Rust port prints the same
    // two warning lines to stderr, which we can't intercept here.)
    write_mcp_config(Location::Local);

    // Backup should exist
    let backup = PathBuf::from(format!("{}.backup", mcp_json.display()));
    assert!(backup.exists());
    // Original backup content should be the corrupted content
    assert!(read(&backup).contains("this is not valid json"));

    // New file should be valid JSON with codegraph config
    let content = read_json(&mcp_json);
    assert!(content["mcpServers"]["codegraph"].is_object());
}

#[test]
fn config_writer_preserves_existing_valid_config() {
    let env = TestEnv::new();
    let mcp_json = env.cwd().join(".mcp.json");
    write(
        &mcp_json,
        &serde_json::to_string_pretty(&json!({
            "mcpServers": { "other": { "command": "other-tool" } },
            "customField": "preserved",
        }))
        .unwrap(),
    );

    write_mcp_config(Location::Local);

    let content = read_json(&mcp_json);
    assert!(content["mcpServers"]["codegraph"].is_object());
    assert!(content["mcpServers"]["other"].is_object());
    assert_eq!(content["customField"], "preserved");
}

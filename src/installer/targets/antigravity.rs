//! Google Antigravity IDE target. Antigravity is Google's VS Code-derived
//! multi-agent IDE; the Gemini CLI is in the process of consolidating with
//! it under a single agent platform. Antigravity reads MCP server
//! definitions from a separate config file from the CLI.
//!
//! ## Config path: unified vs legacy
//!
//! Antigravity recently migrated to a **unified** MCP config path shared
//! across all Antigravity tools:
//!
//!   - **Unified** (post-migration, current): `~/.gemini/config/mcp_config.json`
//!     — signalled by the `~/.gemini/config/.migrated` marker file.
//!   - **Legacy** (pre-migration): `~/.gemini/antigravity/mcp_config.json`
//!     — what the github-mcp-server install guide still documents.
//!
//! We detect the marker at install time and write to the right path. On
//! uninstall we sweep BOTH — so a user who installed on the legacy path,
//! was then auto-migrated by Antigravity, and re-ran `codegraph install`
//! doesn't end up with stale codegraph entries in two files.
//!
//! ## Entry shape: no `type: stdio` field
//!
//! Antigravity rejects MCP entries that carry the `type: "stdio"` field
//! the rest of our targets use — the working entries it manages itself
//! (e.g. `code-review-graph`) omit it, and dropping it was load-bearing
//! to get codegraph to appear in the Customizations UI. We build the
//! entry locally instead of routing through `get_mcp_server_config()`.
//!
//! ## macOS GUI app PATH resolution
//!
//! Antigravity is a GUI Electron app. macOS gives Dock/Finder-launched
//! apps a stripped PATH (`/usr/bin:/bin:/usr/sbin:/sbin`) — nvm-managed
//! tools live outside that, so a bare `codegraph` command fails to spawn
//! even when `which codegraph` resolves in the user's shell. We resolve
//! `codegraph` to its absolute path on macOS at install time. (Linux GUI
//! apps inherit user PATH; Windows uses `PATH` env directly — both are
//! fine with the bare command.)
//!
//! ## Shared instructions (no GEMINI.md from here)
//!
//! The IDE shares `~/.gemini/GEMINI.md` with Gemini CLI for instructions
//! — written by the `./gemini.rs` target. We deliberately don't touch it
//! here so uninstalling Antigravity without uninstalling Gemini CLI
//! leaves CLI instructions intact. Users who install only Antigravity
//! still get a working MCP integration; the prefer-codegraph-over-grep
//! guidance just won't be present unless they also install the gemini
//! target.
//!
//! ## Location
//!
//! `supports_location(Local)` returns false — Antigravity has no
//! project-scoped config concept as of 2026-05.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Map, Value, json};

use super::shared::{home_dir, is_truthy, json_deep_equal, read_json_file, write_json_file};
use super::types::{
    AgentTarget,
    DetectionResult,
    FileAction,
    FileWrite,
    InstallOptions,
    Location,
    TargetId,
    WriteResult,
};

fn unified_config_dir() -> PathBuf {
    home_dir().join(".gemini").join("config")
}

fn unified_mcp_config_path() -> PathBuf {
    unified_config_dir().join("mcp_config.json")
}

fn legacy_config_dir() -> PathBuf {
    home_dir().join(".gemini").join("antigravity")
}

fn legacy_mcp_config_path() -> PathBuf {
    legacy_config_dir().join("mcp_config.json")
}

fn migrated_marker_path() -> PathBuf {
    unified_config_dir().join(".migrated")
}

/// Pick the right MCP config path to write to.
///
/// Prefers the unified `~/.gemini/config/mcp_config.json` when Antigravity
/// has signalled it's migrated (`.migrated` marker present, OR the
/// unified file already exists — Antigravity creates it on first
/// launch post-migration). Falls back to the legacy
/// `~/.gemini/antigravity/mcp_config.json` for users on a pre-migration
/// Antigravity build.
fn preferred_mcp_config_path() -> PathBuf {
    if migrated_marker_path().exists() {
        return unified_mcp_config_path();
    }
    if unified_mcp_config_path().exists() {
        return unified_mcp_config_path();
    }
    legacy_mcp_config_path()
}

/// Resolve the on-disk path of the `codegraph` binary so a Mac GUI app
/// launched from Dock/Finder (with a stripped PATH) can find it. Falls
/// back to the bare `codegraph` name when:
///
///  - we're not on macOS (Linux GUI apps inherit user PATH; Windows
///    uses env PATH directly), OR
///  - the lookup fails for any reason (preserving install in restricted
///    environments where `which`/`command -v` aren't available).
///
/// Resolution prefers `command -v` (built-in, no PATH manipulation),
/// with `which` as a fallback. Both are read via the user's interactive
/// shell PATH at install time — that's the right PATH for finding
/// nvm-managed tools like ours.
fn resolve_codegraph_command() -> String {
    if !cfg!(target_os = "macos") {
        return "codegraph".to_string();
    }
    let output = Command::new("/bin/bash")
        .arg("-c")
        .arg("command -v codegraph || which codegraph")
        .output();
    if let Ok(output) = output {
        if output.status.success() {
            let resolved = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !resolved.is_empty() && Path::new(&resolved).exists() {
                return resolved;
            }
        }
    }
    // fall through to bare name
    "codegraph".to_string()
}

/// Build the codegraph MCP-server entry for Antigravity. Distinct from
/// `get_mcp_server_config()` because Antigravity (a) rejects the `type`
/// field and (b) needs an absolute command path on macOS — see file
/// header.
fn build_antigravity_entry() -> Value {
    json!({
        "command": resolve_codegraph_command(),
        "args": ["serve", "--mcp"],
    })
}

pub struct AntigravityTarget;

impl AgentTarget for AntigravityTarget {
    fn id(&self) -> TargetId {
        TargetId::Antigravity
    }

    fn display_name(&self) -> &'static str {
        "Antigravity IDE"
    }

    fn docs_url(&self) -> Option<&'static str> {
        Some("https://antigravity.google")
    }

    fn supports_location(&self, loc: Location) -> bool {
        loc == Location::Global
    }

    fn detect(&self, loc: Location) -> DetectionResult {
        if loc != Location::Global {
            return DetectionResult {
                installed: false,
                already_configured: false,
                config_path: None,
            };
        }
        let file = preferred_mcp_config_path();
        let config = read_json_file(&file);
        let already_configured = config
            .get("mcpServers")
            .and_then(|m| m.get("codegraph"))
            .map(is_truthy)
            .unwrap_or(false);
        // "Installed" heuristic: either the unified config dir, the legacy
        // config dir, or one of the config files exists. Antigravity creates
        // ~/.gemini/ on first launch even before MCP configs.
        let installed =
            unified_config_dir().exists() || legacy_config_dir().exists() || file.exists();
        DetectionResult {
            installed,
            already_configured,
            config_path: Some(file),
        }
    }

    fn install(&self, loc: Location, _opts: &InstallOptions) -> WriteResult {
        if loc != Location::Global {
            return WriteResult {
                files: Vec::new(),
                notes: vec![
                    "Antigravity IDE has no project-local config — re-run with --location=global."
                        .to_string(),
                ],
            };
        }
        let mut files: Vec<FileWrite> = Vec::new();
        files.push(write_mcp_entry());
        // If the user originally installed on the legacy path and Antigravity
        // has since migrated, strip the stale legacy entry so they don't
        // wind up with two competing codegraph configs.
        if let Some(legacy_cleanup) = cleanup_legacy_entry() {
            files.push(legacy_cleanup);
        }
        WriteResult {
            files,
            notes: vec!["Restart Antigravity for MCP changes to take effect.".to_string()],
        }
    }

    fn uninstall(&self, loc: Location) -> WriteResult {
        if loc != Location::Global {
            return WriteResult::default();
        }
        let mut files: Vec<FileWrite> = Vec::new();

        // Remove from the preferred path.
        let preferred = preferred_mcp_config_path();
        files.push(remove_codegraph_from_file(&preferred));

        // Also sweep the OTHER path (legacy when preferred is unified, and
        // vice versa) — handles the migration-half-state case where codegraph
        // got written to one file but Antigravity now reads from the other.
        let other = if preferred == unified_mcp_config_path() {
            legacy_mcp_config_path()
        } else {
            unified_mcp_config_path()
        };
        if preferred != other {
            let other_result = remove_codegraph_from_file(&other);
            // Only surface the secondary file if we actually touched it —
            // a `not-found` on a file the user never had is noise.
            if other_result.action == FileAction::Removed {
                files.push(other_result);
            }
        }

        WriteResult {
            files,
            notes: Vec::new(),
        }
    }

    fn print_config(&self, loc: Location) -> String {
        if loc != Location::Global {
            return "# Antigravity IDE has no project-local config — use --location=global.\n"
                .to_string();
        }
        let file = preferred_mcp_config_path();
        let snippet = serde_json::to_string_pretty(&json!({
            "mcpServers": { "codegraph": build_antigravity_entry() }
        }))
        .unwrap_or_default();
        format!("# Add to {}\n\n{}\n", file.display(), snippet)
    }

    fn describe_paths(&self, loc: Location) -> Vec<PathBuf> {
        if loc != Location::Global {
            return Vec::new();
        }
        vec![preferred_mcp_config_path()]
    }
}

fn write_mcp_entry() -> FileWrite {
    let file = preferred_mcp_config_path();
    if let Some(dir) = file.parent() {
        if !dir.exists() {
            let _ = fs::create_dir_all(dir);
        }
    }

    let mut existing = read_json_file(&file);
    let before = existing
        .get("mcpServers")
        .and_then(|m| m.get("codegraph"))
        .cloned();
    let after = build_antigravity_entry();

    if let Some(before_val) = &before {
        if json_deep_equal(before_val, &after) {
            return FileWrite {
                path: file,
                action: FileAction::Unchanged,
            };
        }
    }
    // Mirrors the TS chained ternary `before ? 'updated' : (exists ? 'updated'
    // : 'created')` — the two 'updated' arms are deliberately distinct cases.
    #[allow(clippy::if_same_then_else)]
    let action = if before.map(|b| is_truthy(&b)).unwrap_or(false) {
        FileAction::Updated
    } else if file.exists() {
        FileAction::Updated
    } else {
        FileAction::Created
    };
    if !matches!(existing.get("mcpServers"), Some(Value::Object(_))) {
        existing.insert("mcpServers".to_string(), Value::Object(Map::new()));
    }
    if let Some(Value::Object(servers)) = existing.get_mut("mcpServers") {
        servers.insert("codegraph".to_string(), after);
    }
    write_json_file(&file, &existing);
    FileWrite { path: file, action }
}

/// Strip the codegraph entry from the legacy `~/.gemini/antigravity/mcp_config.json`
/// if it's present AND we're writing to the unified path. Used by install
/// to migrate users who had codegraph configured on the legacy path
/// before Antigravity migrated their config. Returns the file action for
/// reporting, or `None` when there's nothing to clean up.
fn cleanup_legacy_entry() -> Option<FileWrite> {
    if preferred_mcp_config_path() != unified_mcp_config_path() {
        return None;
    }
    let legacy = legacy_mcp_config_path();
    if !legacy.exists() {
        return None;
    }
    let mut config = read_json_file(&legacy);
    let has_codegraph = config
        .get("mcpServers")
        .and_then(|m| m.get("codegraph"))
        .map(is_truthy)
        .unwrap_or(false);
    if !has_codegraph {
        return None;
    }
    if let Some(Value::Object(servers)) = config.get_mut("mcpServers") {
        servers.remove("codegraph");
        if servers.is_empty() {
            config.remove("mcpServers");
        }
    }
    write_json_file(&legacy, &config);
    Some(FileWrite {
        path: legacy,
        action: FileAction::Removed,
    })
}

fn remove_codegraph_from_file(file: &Path) -> FileWrite {
    if !file.exists() {
        return FileWrite {
            path: file.to_path_buf(),
            action: FileAction::NotFound,
        };
    }
    let mut config = read_json_file(file);
    let has_codegraph = config
        .get("mcpServers")
        .and_then(|m| m.get("codegraph"))
        .map(is_truthy)
        .unwrap_or(false);
    if !has_codegraph {
        return FileWrite {
            path: file.to_path_buf(),
            action: FileAction::NotFound,
        };
    }
    if let Some(Value::Object(servers)) = config.get_mut("mcpServers") {
        servers.remove("codegraph");
        if servers.is_empty() {
            config.remove("mcpServers");
        }
    }
    // Leave a now-empty `{}` in place — Antigravity manages this file and
    // a stray empty file is less surprising than a deletion.
    write_json_file(file, &config);
    FileWrite {
        path: file.to_path_buf(),
        action: FileAction::Removed,
    }
}

pub static ANTIGRAVITY_TARGET: AntigravityTarget = AntigravityTarget;

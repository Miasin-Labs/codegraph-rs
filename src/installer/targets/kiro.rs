//! Kiro CLI / IDE target. Writes:
//!
//!   - MCP server entry to `~/.kiro/settings/mcp.json` (global) or
//!     `./.kiro/settings/mcp.json` (local). Standard `mcpServers.codegraph`
//!     shape, same as Claude / Cursor / Gemini.
//!   - Instructions to `~/.kiro/steering/codegraph.md` (global) or
//!     `./.kiro/steering/codegraph.md` (local). Kiro's "steering" system
//!     loads every `*.md` file in the steering dir as agent context, so
//!     a dedicated `codegraph.md` is the natural surface — we own the
//!     whole file outright (no marker-based merging needed) and delete
//!     it on uninstall.
//!
//! No permissions concept — Kiro gates tool invocations through its own
//! UI prompts rather than an external allowlist. `auto_allow` is silently
//! ignored.
//!
//! Paths are identical on macOS / Linux / Windows because Kiro resolves
//! its config root from the home dir on all three (Windows `~` →
//! `%USERPROFILE%\.kiro`).
//!
//! Docs: https://kiro.dev/docs/cli/mcp/
//!       https://kiro.dev/docs/cli/steering/

use std::fs;
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::shared::{
    cwd,
    get_mcp_server_config,
    home_dir,
    is_truthy,
    json_deep_equal,
    read_json_file,
    write_json_file,
};
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

fn config_dir(loc: Location) -> PathBuf {
    match loc {
        Location::Global => home_dir().join(".kiro"),
        Location::Local => cwd().join(".kiro"),
    }
}

fn mcp_json_path(loc: Location) -> PathBuf {
    config_dir(loc).join("settings").join("mcp.json")
}

fn steering_path(loc: Location) -> PathBuf {
    config_dir(loc).join("steering").join("codegraph.md")
}

pub struct KiroTarget;

impl AgentTarget for KiroTarget {
    fn id(&self) -> TargetId {
        TargetId::Kiro
    }

    fn display_name(&self) -> &'static str {
        "Kiro"
    }

    fn docs_url(&self) -> Option<&'static str> {
        Some("https://kiro.dev/docs/cli/mcp/")
    }

    fn supports_location(&self, _loc: Location) -> bool {
        true
    }

    fn detect(&self, loc: Location) -> DetectionResult {
        let file = mcp_json_path(loc);
        let config = read_json_file(&file);
        let already_configured = config
            .get("mcpServers")
            .and_then(|m| m.get("codegraph"))
            .map(is_truthy)
            .unwrap_or(false);
        let installed = match loc {
            Location::Global => config_dir(Location::Global).exists() || file.exists(),
            Location::Local => file.exists() || config_dir(Location::Local).exists(),
        };
        DetectionResult {
            installed,
            already_configured,
            config_path: Some(file),
        }
    }

    fn install(&self, loc: Location, _opts: &InstallOptions) -> WriteResult {
        let mut files: Vec<FileWrite> = Vec::new();
        files.push(write_mcp_entry(loc));

        // The steering doc is no longer written — the codegraph usage
        // guidance ships in the MCP server's `initialize` response (issue
        // #529). Delete a `codegraph.md` a previous install created so an
        // upgrade self-heals.
        let steering_cleanup = remove_steering_entry(loc);
        if steering_cleanup.action == FileAction::Removed {
            files.push(steering_cleanup);
        }

        WriteResult {
            files,
            // The IDE-only enable-MCP step is load-bearing: Kiro IDE ships
            // with MCP support disabled by default, so even a valid
            // `~/.kiro/settings/mcp.json` at the documented path is ignored
            // until the user flips the toggle. Kiro CLI reads the same file
            // without a gate, so we call out which audience this applies to.
            notes: vec![
                "Restart Kiro for MCP changes to take effect.".to_string(),
                "Kiro IDE: also enable MCP in Settings (search \"MCP\" → \"Enabled\"). Kiro CLI users can skip this step.".to_string(),
            ],
        }
    }

    fn uninstall(&self, loc: Location) -> WriteResult {
        let mut files: Vec<FileWrite> = Vec::new();

        let file = mcp_json_path(loc);
        let mut config = read_json_file(&file);
        let has_codegraph = config
            .get("mcpServers")
            .and_then(|m| m.get("codegraph"))
            .map(is_truthy)
            .unwrap_or(false);
        if has_codegraph {
            if let Some(Value::Object(servers)) = config.get_mut("mcpServers") {
                servers.remove("codegraph");
                if servers.is_empty() {
                    config.remove("mcpServers");
                }
            }
            write_json_file(&file, &config);
            files.push(FileWrite {
                path: file,
                action: FileAction::Removed,
            });
        } else {
            files.push(FileWrite {
                path: file,
                action: FileAction::NotFound,
            });
        }

        files.push(remove_steering_entry(loc));

        WriteResult {
            files,
            notes: Vec::new(),
        }
    }

    fn print_config(&self, loc: Location) -> String {
        let target = mcp_json_path(loc);
        let snippet = serde_json::to_string_pretty(&json!({
            "mcpServers": { "codegraph": get_mcp_server_config() }
        }))
        .unwrap_or_default();
        format!("# Add to {}\n\n{}\n", target.display(), snippet)
    }

    fn describe_paths(&self, loc: Location) -> Vec<PathBuf> {
        vec![mcp_json_path(loc), steering_path(loc)]
    }
}

fn write_mcp_entry(loc: Location) -> FileWrite {
    let file = mcp_json_path(loc);
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
    let after = get_mcp_server_config();

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

/// Delete the steering file we own. If a user has hand-edited the file
/// out of recognition we still remove it — codegraph.md is a name we
/// claim, and a partial install leaving the file behind is worse than
/// a clean delete. Used by both install (self-heal on upgrade — see
/// issue #529) and uninstall.
fn remove_steering_entry(loc: Location) -> FileWrite {
    let file = steering_path(loc);
    if !file.exists() {
        return FileWrite {
            path: file,
            action: FileAction::NotFound,
        };
    }
    let _ = fs::remove_file(&file); // ignore
    FileWrite {
        path: file,
        action: FileAction::Removed,
    }
}

pub static KIRO_TARGET: KiroTarget = KiroTarget;

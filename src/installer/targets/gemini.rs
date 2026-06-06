//! Gemini CLI target (also covers the rebranded "Antigravity CLI" —
//! Google is in the middle of unifying its CLI tools under
//! Antigravity, and the new CLI continues to read `~/.gemini/settings.json`
//! + project-local `.gemini/settings.json`). Writes:
//!
//!   - MCP server entry to `~/.gemini/settings.json` (global) or
//!     `./.gemini/settings.json` (local) under the standard
//!     `mcpServers.codegraph` key. Same shape as Claude / Cursor.
//!   - Instructions to `~/.gemini/GEMINI.md` (global) or `./GEMINI.md`
//!     (local — Gemini reads the project root file directly, not
//!     under `.gemini/`).
//!
//! No permissions concept — Gemini CLI gates tool invocations through
//! the `trust` field per server, not an external allowlist. We leave
//! `trust` unset so the user controls confirmation prompts.
//!
//! The Antigravity IDE shares `~/.gemini/GEMINI.md` for instructions
//! but uses a separate MCP config file (`~/.gemini/antigravity/mcp_config.json`)
//! — see `./antigravity.rs`. Both targets writing to GEMINI.md is
//! safe: the marker-based section replacement makes the second write
//! a byte-identical no-op.

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
    remove_marked_section,
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
use crate::installer::instructions_template::{CODEGRAPH_SECTION_END, CODEGRAPH_SECTION_START};

fn config_dir(loc: Location) -> PathBuf {
    match loc {
        Location::Global => home_dir().join(".gemini"),
        Location::Local => cwd().join(".gemini"),
    }
}

fn settings_json_path(loc: Location) -> PathBuf {
    config_dir(loc).join("settings.json")
}

fn instructions_path(loc: Location) -> PathBuf {
    // Global GEMINI.md lives under ~/.gemini/; project-local GEMINI.md
    // lives at the project root (NOT under .gemini/), matching how
    // Gemini CLI's hierarchical context loader searches.
    match loc {
        Location::Global => config_dir(Location::Global).join("GEMINI.md"),
        Location::Local => cwd().join("GEMINI.md"),
    }
}

pub struct GeminiTarget;

impl AgentTarget for GeminiTarget {
    fn id(&self) -> TargetId {
        TargetId::Gemini
    }

    fn display_name(&self) -> &'static str {
        "Gemini CLI"
    }

    fn docs_url(&self) -> Option<&'static str> {
        Some("https://geminicli.com/docs/tools/mcp-server/")
    }

    fn supports_location(&self, _loc: Location) -> bool {
        true
    }

    fn detect(&self, loc: Location) -> DetectionResult {
        let file = settings_json_path(loc);
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

        // GEMINI.md is no longer written — the codegraph usage guidance
        // ships in the MCP server's `initialize` response (issue #529).
        // Strip a block a previous install left so an upgrade self-heals.
        let instr_cleanup = remove_instructions_entry(loc);
        if instr_cleanup.action == FileAction::Removed {
            files.push(instr_cleanup);
        }

        WriteResult {
            files,
            notes: Vec::new(),
        }
    }

    fn uninstall(&self, loc: Location) -> WriteResult {
        let mut files: Vec<FileWrite> = Vec::new();

        let file = settings_json_path(loc);
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
            // If the file is now an empty `{}` we still leave it — other
            // (top-level) Gemini settings the user might add later can
            // share the file; deleting it would be surprising.
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

        files.push(remove_instructions_entry(loc));

        WriteResult {
            files,
            notes: Vec::new(),
        }
    }

    fn print_config(&self, loc: Location) -> String {
        let target = settings_json_path(loc);
        let snippet = serde_json::to_string_pretty(&json!({
            "mcpServers": { "codegraph": get_mcp_server_config() }
        }))
        .unwrap_or_default();
        format!("# Add to {}\n\n{}\n", target.display(), snippet)
    }

    fn describe_paths(&self, loc: Location) -> Vec<PathBuf> {
        vec![settings_json_path(loc), instructions_path(loc)]
    }
}

fn write_mcp_entry(loc: Location) -> FileWrite {
    let file = settings_json_path(loc);
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

/// Strip the marker-delimited CodeGraph block from GEMINI.md if a prior
/// install wrote one. Used by both install (self-heal on upgrade) and
/// uninstall — see issue #529.
fn remove_instructions_entry(loc: Location) -> FileWrite {
    let file = instructions_path(loc);
    let action = remove_marked_section(&file, CODEGRAPH_SECTION_START, CODEGRAPH_SECTION_END);
    FileWrite { path: file, action }
}

pub static GEMINI_TARGET: GeminiTarget = GeminiTarget;

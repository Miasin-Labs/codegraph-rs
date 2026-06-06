//! OpenAI Codex CLI target.
//!
//!   - MCP server entry to `~/.codex/config.toml` as the dotted-key
//!     table `[mcp_servers.codegraph]`. TOML — not JSON — handled by
//!     the narrow serializer in `./toml.rs`.
//!   - Instructions to `~/.codex/AGENTS.md`.
//!
//! Codex CLI as of 2026-05 has no project-local config concept —
//! everything lives under `~/.codex/`. `supports_location(Local)`
//! returns false; the orchestrator skips Codex when the user picks
//! the local install location.
//!
//! No permissions concept.

use std::fs;
use std::path::PathBuf;

use super::shared::{
    atomic_write_file_sync,
    get_mcp_server_config,
    home_dir,
    remove_marked_section,
};
use super::toml::{
    TomlRemoveAction,
    TomlUpsertAction,
    TomlValue,
    build_toml_table,
    remove_toml_table,
    upsert_toml_table,
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

const TOML_HEADER: &str = "mcp_servers.codegraph";

fn config_dir() -> PathBuf {
    home_dir().join(".codex")
}

fn toml_config_path() -> PathBuf {
    config_dir().join("config.toml")
}

fn instructions_path() -> PathBuf {
    config_dir().join("AGENTS.md")
}

pub struct CodexTarget;

impl AgentTarget for CodexTarget {
    fn id(&self) -> TargetId {
        TargetId::Codex
    }

    fn display_name(&self) -> &'static str {
        "Codex CLI"
    }

    fn docs_url(&self) -> Option<&'static str> {
        Some("https://github.com/openai/codex")
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
        let toml_path = toml_config_path();
        let mut already_configured = false;
        if toml_path.exists() {
            if let Ok(content) = fs::read_to_string(&toml_path) {
                already_configured = content.contains(&format!("[{TOML_HEADER}]"));
            }
        }
        let installed = config_dir().exists();
        DetectionResult {
            installed,
            already_configured,
            config_path: Some(toml_path),
        }
    }

    fn install(&self, loc: Location, _opts: &InstallOptions) -> WriteResult {
        if loc != Location::Global {
            return WriteResult {
                files: Vec::new(),
                notes: vec![
                    "Codex CLI has no project-local config — re-run with --location=global to install."
                        .to_string(),
                ],
            };
        }
        let mut files: Vec<FileWrite> = Vec::new();

        files.push(write_mcp_entry());

        // AGENTS.md is no longer written — the codegraph usage guidance
        // ships in the MCP server's `initialize` response (issue #529).
        // Strip a block a previous install left so an upgrade self-heals.
        let instr_cleanup = remove_instructions_entry();
        if instr_cleanup.action == FileAction::Removed {
            files.push(instr_cleanup);
        }

        WriteResult {
            files,
            notes: Vec::new(),
        }
    }

    fn uninstall(&self, loc: Location) -> WriteResult {
        if loc != Location::Global {
            return WriteResult::default();
        }
        let mut files: Vec<FileWrite> = Vec::new();

        let toml_path = toml_config_path();
        if toml_path.exists() {
            let content = fs::read_to_string(&toml_path).unwrap_or_default();
            let (next_content, action) = remove_toml_table(&content, TOML_HEADER);
            if action == TomlRemoveAction::Removed {
                if next_content.trim().is_empty() {
                    let _ = fs::remove_file(&toml_path); // ignore
                } else {
                    let _ = atomic_write_file_sync(
                        &toml_path,
                        &format!("{}\n", next_content.trim_end()),
                    );
                }
                files.push(FileWrite {
                    path: toml_path,
                    action: FileAction::Removed,
                });
            } else {
                files.push(FileWrite {
                    path: toml_path,
                    action: FileAction::NotFound,
                });
            }
        } else {
            files.push(FileWrite {
                path: toml_path,
                action: FileAction::NotFound,
            });
        }

        files.push(remove_instructions_entry());

        WriteResult {
            files,
            notes: Vec::new(),
        }
    }

    fn print_config(&self, loc: Location) -> String {
        if loc != Location::Global {
            return "# Codex CLI has no project-local config — use --location=global.\n"
                .to_string();
        }
        let block = build_codegraph_block();
        format!("# Add to {}\n\n{}\n", toml_config_path().display(), block)
    }

    fn describe_paths(&self, loc: Location) -> Vec<PathBuf> {
        if loc != Location::Global {
            return Vec::new();
        }
        vec![toml_config_path(), instructions_path()]
    }
}

fn build_codegraph_block() -> String {
    let mcp = get_mcp_server_config();
    let command = mcp["command"].as_str().unwrap_or("codegraph").to_string();
    let args: Vec<String> = mcp["args"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    build_toml_table(
        TOML_HEADER,
        &[
            ("command", TomlValue::String(command)),
            ("args", TomlValue::Array(args)),
        ],
    )
}

fn write_mcp_entry() -> FileWrite {
    let file = toml_config_path();
    if let Some(dir) = file.parent() {
        if !dir.exists() {
            let _ = fs::create_dir_all(dir);
        }
    }

    let block = build_codegraph_block();
    // Single read — `existing.is_empty()` derives both "is the file empty
    // or absent" and "what was its content," avoiding a TOCTOU window
    // between two existence checks.
    let existing = if file.exists() {
        fs::read_to_string(&file).unwrap_or_default()
    } else {
        String::new()
    };
    let created = existing.is_empty();
    let (next_content, action) = upsert_toml_table(&existing, TOML_HEADER, &block);

    if action == TomlUpsertAction::Unchanged {
        return FileWrite {
            path: file,
            action: FileAction::Unchanged,
        };
    }
    let _ = atomic_write_file_sync(&file, &next_content);
    FileWrite {
        path: file,
        action: if created {
            FileAction::Created
        } else {
            FileAction::Updated
        },
    }
}

/// Strip the marker-delimited CodeGraph block from `~/.codex/AGENTS.md`
/// if a prior install wrote one. Used by both install (self-heal on
/// upgrade) and uninstall — see issue #529.
fn remove_instructions_entry() -> FileWrite {
    let file = instructions_path();
    let action = remove_marked_section(&file, CODEGRAPH_SECTION_START, CODEGRAPH_SECTION_END);
    FileWrite { path: file, action }
}

pub static CODEX_TARGET: CodexTarget = CodexTarget;

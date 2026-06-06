//! Cursor target.
//!
//!   - MCP server entry to `~/.cursor/mcp.json` (global) or
//!     `./.cursor/mcp.json` (local). Same `{mcpServers: {...}}` shape
//!     as Claude.
//!   - Instructions to `./.cursor/rules/codegraph.mdc` (project-local
//!     ONLY). Cursor's rules system is a project-scoped surface;
//!     global cursor rules aren't a stable convention as of 2026-05.
//!     For `--location=global`, only mcp.json is written.
//!
//! ## Why we hardcode `--path` for Cursor
//!
//! Cursor launches MCP-server subprocesses with a working directory
//! that ISN'T the workspace root AND doesn't pass `rootUri` /
//! `workspaceFolders` in the MCP initialize call. The codegraph MCP
//! server's cwd fallback therefore misses the workspace's
//! `.codegraph/` and reports "not initialized" on every tool call.
//!
//! So we inject `--path` into the args ourselves:
//!
//!   - `local`  install: absolute path (we know it at install time).
//!   - `global` install: `${workspaceFolder}` — Cursor expands this to
//!     the open workspace's root, giving us per-workspace behavior
//!     from a single global config.
//!
//! Codex and Claude do not need this — they launch MCP servers with
//! `cwd = workspace` and pass `rootUri`, respectively.
//!
//! No permissions concept — Cursor doesn't have an auto-allow list
//! the installer can populate. `auto_allow` is silently ignored.

use std::fs;
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::shared::{
    atomic_write_file_sync,
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
use crate::installer::instructions_template::{CODEGRAPH_SECTION_END, CODEGRAPH_SECTION_START};

fn mcp_json_path(loc: Location) -> PathBuf {
    match loc {
        Location::Global => home_dir().join(".cursor").join("mcp.json"),
        Location::Local => cwd().join(".cursor").join("mcp.json"),
    }
}

/// Cursor "rules" file. Only meaningful for the project-local
/// location — Cursor reads `.cursor/rules/*.mdc` from the workspace
/// root. There is no global equivalent.
fn rules_path() -> PathBuf {
    cwd().join(".cursor").join("rules").join("codegraph.mdc")
}

/// Cursor `.mdc` rules use YAML-ish frontmatter. `alwaysApply: true`
/// makes the rule load on every conversation regardless of file
/// patterns — appropriate for a tool-usage guide that's relevant
/// whenever the user is asking the agent to navigate code.
const MDC_FRONTMATTER: &str = "---\ndescription: CodeGraph MCP usage guide — when to use which tool\nalwaysApply: true\n---\n";

pub struct CursorTarget;

impl AgentTarget for CursorTarget {
    fn id(&self) -> TargetId {
        TargetId::Cursor
    }

    fn display_name(&self) -> &'static str {
        "Cursor"
    }

    fn docs_url(&self) -> Option<&'static str> {
        Some("https://docs.cursor.com/context/model-context-protocol")
    }

    fn supports_location(&self, _loc: Location) -> bool {
        // Both supported, but `local` writes more files (mcp.json + rules);
        // `global` writes only mcp.json. The orchestrator surfaces the
        // difference via describe_paths.
        true
    }

    fn detect(&self, loc: Location) -> DetectionResult {
        let mcp_path = mcp_json_path(loc);
        let config = read_json_file(&mcp_path);
        let already_configured = config
            .get("mcpServers")
            .and_then(|m| m.get("codegraph"))
            .map(is_truthy)
            .unwrap_or(false);
        // "Installed" heuristic: does ~/.cursor exist (global) or has the
        // user opted into a project-local cursor config dir?
        let installed = match loc {
            Location::Global => home_dir().join(".cursor").exists(),
            Location::Local => cwd().join(".cursor").exists(),
        };
        DetectionResult {
            installed,
            already_configured,
            config_path: Some(mcp_path),
        }
    }

    fn install(&self, loc: Location, _opts: &InstallOptions) -> WriteResult {
        let mut files: Vec<FileWrite> = Vec::new();

        files.push(write_mcp_entry(loc));

        // We no longer write `.cursor/rules/codegraph.mdc` — the codegraph
        // usage guidance ships in the MCP server's `initialize` response,
        // the single source of truth (issue #529). Strip a rules file a
        // previous install created so an upgrade self-heals.
        if loc == Location::Local {
            let rules_cleanup = remove_rules_entry();
            if rules_cleanup.action == FileAction::Removed {
                files.push(rules_cleanup);
            }
        }

        WriteResult {
            files,
            notes: vec!["Restart Cursor for MCP changes to take effect.".to_string()],
        }
    }

    fn uninstall(&self, loc: Location) -> WriteResult {
        let mut files: Vec<FileWrite> = Vec::new();

        let mcp_path = mcp_json_path(loc);
        let mut config = read_json_file(&mcp_path);
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
            write_json_file(&mcp_path, &config);
            files.push(FileWrite {
                path: mcp_path,
                action: FileAction::Removed,
            });
        } else {
            files.push(FileWrite {
                path: mcp_path,
                action: FileAction::NotFound,
            });
        }

        if loc == Location::Local {
            files.push(remove_rules_entry());
        }

        WriteResult {
            files,
            notes: Vec::new(),
        }
    }

    fn print_config(&self, loc: Location) -> String {
        let target = mcp_json_path(loc);
        let snippet = serde_json::to_string_pretty(&json!({
            "mcpServers": { "codegraph": build_cursor_mcp_config(loc) }
        }))
        .unwrap_or_default();
        format!("# Add to {}\n\n{}\n", target.display(), snippet)
    }

    fn describe_paths(&self, loc: Location) -> Vec<PathBuf> {
        match loc {
            Location::Local => vec![mcp_json_path(loc), rules_path()],
            Location::Global => vec![mcp_json_path(loc)],
        }
    }
}

/// Build the codegraph MCP-server config for Cursor at the given
/// location. Inherits the shared shape ({type, command, args}) and
/// appends `--path` so the spawned MCP server resolves the workspace
/// correctly regardless of Cursor's launch cwd. See file header for
/// the full rationale.
fn build_cursor_mcp_config(loc: Location) -> Value {
    let mut base = get_mcp_server_config();
    let path_arg = match loc {
        Location::Local => cwd().to_string_lossy().into_owned(),
        Location::Global => "${workspaceFolder}".to_string(),
    };
    if let Some(Value::Array(args)) = base.get_mut("args") {
        args.push(Value::String("--path".to_string()));
        args.push(Value::String(path_arg));
    }
    base
}

fn write_mcp_entry(loc: Location) -> FileWrite {
    let file = mcp_json_path(loc);
    let mut existing = read_json_file(&file);
    let before = existing
        .get("mcpServers")
        .and_then(|m| m.get("codegraph"))
        .cloned();
    let after = build_cursor_mcp_config(loc);

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

/// Remove the Cursor rules file on uninstall (and as a self-heal on
/// install — see issue #529).
///
/// Unlike the shared CLAUDE.md / AGENTS.md files (where codegraph owns
/// only a marker-delimited section), `.cursor/rules/codegraph.mdc` is a
/// file we create OUTRIGHT — the frontmatter is ours too. So a plain
/// `remove_marked_section` is wrong here: it would strip our instruction
/// block but leave the orphaned `description: CodeGraph ...` frontmatter
/// behind, so the file lingers and still "mentions" codegraph.
///
/// Instead: strip our block, and if nothing but our own frontmatter
/// remains, delete the whole file. Only when the user has added their
/// own content outside our markers do we keep the file (minus our block).
fn remove_rules_entry() -> FileWrite {
    let file = rules_path();
    if !file.exists() {
        return FileWrite {
            path: file,
            action: FileAction::NotFound,
        };
    }

    let content = match fs::read_to_string(&file) {
        Ok(c) => c,
        Err(_) => {
            return FileWrite {
                path: file,
                action: FileAction::NotFound,
            };
        }
    };

    let our_frontmatter = MDC_FRONTMATTER.trim();
    let start_idx = content.find(CODEGRAPH_SECTION_START);
    let end_idx = content.find(CODEGRAPH_SECTION_END);

    // Our marked block is present — strip it, then decide what's left.
    if let (Some(start_idx), Some(end_idx)) = (start_idx, end_idx) {
        if end_idx > start_idx {
            let before = content[..start_idx].trim_end();
            let after = content[end_idx + CODEGRAPH_SECTION_END.len()..].trim_start();
            let sep = if !before.is_empty() && !after.is_empty() {
                "\n\n"
            } else {
                ""
            };
            let remainder = format!("{before}{sep}{after}");
            let remainder = remainder.trim();
            if remainder.is_empty() || remainder == our_frontmatter {
                let _ = fs::remove_file(&file); // ignore
            } else {
                let _ = atomic_write_file_sync(&file, &format!("{remainder}\n"));
            }
            return FileWrite {
                path: file,
                action: FileAction::Removed,
            };
        }
    }

    // No block, but the file is still our pristine frontmatter-only file
    // — it's ours, so remove it.
    if content.trim() == our_frontmatter {
        let _ = fs::remove_file(&file); // ignore
        return FileWrite {
            path: file,
            action: FileAction::Removed,
        };
    }

    // Foreign content we don't recognize — leave it alone.
    FileWrite {
        path: file,
        action: FileAction::NotFound,
    }
}

pub static CURSOR_TARGET: CursorTarget = CursorTarget;

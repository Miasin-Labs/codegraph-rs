//! Claude Code target. Writes:
//!
//!   - MCP server entry to `~/.claude.json` (global = user scope, loads
//!     in every project) or `./.mcp.json` (local = project scope, the
//!     file Claude Code actually reads for a single project). See the
//!     scope table at https://code.claude.com/docs/en/mcp.
//!   - Permissions to `~/.claude/settings.json` (global) or
//!     `./.claude/settings.json` (local), gated on `auto_allow`.
//!   - Instructions to `~/.claude/CLAUDE.md` (global) or
//!     `./.claude/CLAUDE.md` (local).
//!
//! Earlier versions wrote the local MCP entry to `./.claude.json` — a
//! file Claude Code never reads — so the server silently never loaded
//! until the user manually renamed it to `.mcp.json` (issue #207). We
//! now write `./.mcp.json` and migrate any stale `./.claude.json` entry
//! out of the way on install and uninstall.

use std::fs;
use std::path::PathBuf;

use serde_json::{Map, Value};

use super::shared::{
    cwd,
    get_codegraph_permissions,
    get_mcp_server_config,
    home_dir,
    is_truthy,
    json_deep_equal,
    read_json_file,
    remove_marked_section,
    upsert_instructions_entry,
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
        Location::Global => home_dir().join(".claude"),
        Location::Local => cwd().join(".claude"),
    }
}

fn mcp_json_path(loc: Location) -> PathBuf {
    // global → ~/.claude.json (user scope: visible in every project).
    // local  → ./.mcp.json (project scope: the ONLY project-level MCP
    // file Claude Code reads — NOT ./.claude.json, which it ignores).
    match loc {
        Location::Global => home_dir().join(".claude.json"),
        Location::Local => cwd().join(".mcp.json"),
    }
}

/// Where pre-#207 installers wrote the local MCP entry. Claude Code
/// never reads a project-level `./.claude.json`, so we migrate the
/// codegraph entry out of it on install and strip it on uninstall.
/// Only the project-local path is legacy — global `~/.claude.json` is
/// the correct user-scope location and is left untouched.
fn legacy_local_mcp_path() -> PathBuf {
    cwd().join(".claude.json")
}

fn settings_json_path(loc: Location) -> PathBuf {
    config_dir(loc).join("settings.json")
}

fn instructions_path(loc: Location) -> PathBuf {
    config_dir(loc).join("CLAUDE.md")
}

pub struct ClaudeCodeTarget;

impl AgentTarget for ClaudeCodeTarget {
    fn id(&self) -> TargetId {
        TargetId::Claude
    }

    fn display_name(&self) -> &'static str {
        "Claude Code"
    }

    fn docs_url(&self) -> Option<&'static str> {
        Some("https://docs.claude.com/en/docs/claude-code")
    }

    fn supports_location(&self, _loc: Location) -> bool {
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
        // For "installed" we infer from the existence of either the dir
        // (global) or the project marker file (local). Cheap and avoids
        // shelling out to `claude --version`.
        let installed = match loc {
            Location::Global => config_dir(loc).exists() || mcp_path.exists(),
            Location::Local => mcp_path.exists() || config_dir(loc).exists(),
        };
        DetectionResult {
            installed,
            already_configured,
            config_path: Some(mcp_path),
        }
    }

    fn install(&self, loc: Location, opts: &InstallOptions) -> WriteResult {
        let mut files: Vec<FileWrite> = Vec::new();

        // 1. MCP server entry
        files.push(write_mcp_entry(loc));

        // 1b. Migrate away any stale ./.claude.json left by a pre-#207
        // local install, so the project isn't left with two competing
        // (one dead) MCP configs.
        if loc == Location::Local {
            if let Some(migrated) = cleanup_legacy_local_mcp() {
                files.push(migrated);
            }
        }

        // 2. Permissions (only when auto_allow)
        if opts.auto_allow {
            files.push(write_permissions_entry(loc));
        }

        // 2b. Strip stale auto-sync hooks left by a pre-0.8 install. Those
        // versions wrote `codegraph mark-dirty` / `sync-if-dirty` hooks to
        // settings.json; both subcommands are gone from the CLI, so the
        // Stop hook now fails every turn with "unknown command
        // 'sync-if-dirty'". Cleaning up on install makes an upgrade
        // self-healing. Only surfaced when something was actually removed.
        let hook_cleanup = cleanup_legacy_hooks(loc);
        if hook_cleanup.action == FileAction::Removed {
            files.push(hook_cleanup);
        }

        // 2c. Front-load structural prompts through Claude's
        // UserPromptSubmit hook. `None` deliberately leaves an existing
        // choice alone for callers that do not manage this option.
        match opts.prompt_hook {
            Some(true) => files.push(write_prompt_hook_entry(loc)),
            Some(false) => {
                let removed = remove_prompt_hook_entry(loc);
                if removed.action == FileAction::Removed {
                    files.push(removed);
                }
            }
            None => {}
        }

        // 3. Short CLAUDE.md guidance for Task-tool subagents and non-MCP
        // harnesses, which never receive the MCP initialize instructions.
        // Marker replacement self-heals the old long block and is idempotent.
        files.push(upsert_instructions_entry(&instructions_path(loc)));

        WriteResult {
            files,
            notes: Vec::new(),
        }
    }

    fn uninstall(&self, loc: Location) -> WriteResult {
        let mut files: Vec<FileWrite> = Vec::new();

        // 1. MCP server entry
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

        // 1b. Also strip the codegraph entry from a legacy ./.claude.json
        // so uninstall fully reverses a pre-#207 local install.
        if loc == Location::Local {
            if let Some(migrated) = cleanup_legacy_local_mcp() {
                files.push(migrated);
            }
        }

        // 2. Permissions
        let settings_path = settings_json_path(loc);
        let mut settings = read_json_file(&settings_path);
        let allow_is_array = matches!(
            settings.get("permissions").and_then(|p| p.get("allow")),
            Some(Value::Array(_))
        );
        if allow_is_array {
            let mut changed = false;
            let mut allow_empty = false;
            if let Some(Value::Object(permissions)) = settings.get_mut("permissions") {
                if let Some(Value::Array(allow)) = permissions.get_mut("allow") {
                    let before = allow.len();
                    allow.retain(|p| {
                        !p.as_str()
                            .map(|s| s.starts_with("mcp__codegraph__"))
                            .unwrap_or(false)
                    });
                    if allow.len() != before {
                        changed = true;
                        allow_empty = allow.is_empty();
                    }
                }
                if changed && allow_empty {
                    permissions.remove("allow");
                }
            }
            if changed {
                let perms_now_empty = matches!(
                    settings.get("permissions"),
                    Some(Value::Object(p)) if p.is_empty()
                );
                if perms_now_empty {
                    settings.remove("permissions");
                }
                write_json_file(&settings_path, &settings);
                files.push(FileWrite {
                    path: settings_path,
                    action: FileAction::Removed,
                });
            } else {
                files.push(FileWrite {
                    path: settings_path,
                    action: FileAction::NotFound,
                });
            }
        } else {
            files.push(FileWrite {
                path: settings_path,
                action: FileAction::NotFound,
            });
        }

        // 2b. Strip any stale auto-sync hooks a pre-0.8 install left in
        // settings.json. The hook-cleanup step was lost when the installer
        // moved to the per-target architecture; restoring it here means
        // uninstall — and the npm `preuninstall` hook that drives it — fully
        // reverses a legacy install.
        let hook_cleanup = cleanup_legacy_hooks(loc);
        if hook_cleanup.action == FileAction::Removed {
            files.push(hook_cleanup);
        }

        // 2c. Remove the UserPromptSubmit hook this installer may have added.
        let prompt_hook_cleanup = remove_prompt_hook_entry(loc);
        if prompt_hook_cleanup.action == FileAction::Removed {
            files.push(prompt_hook_cleanup);
        }

        // 3. Instructions — strip only the marker-fenced CodeGraph block.
        files.push(remove_instructions_entry(loc));

        WriteResult {
            files,
            notes: Vec::new(),
        }
    }

    fn print_config(&self, loc: Location) -> String {
        let target = mcp_json_path(loc);
        let snippet = serde_json::to_string_pretty(&serde_json::json!({
            "mcpServers": { "codegraph": get_mcp_server_config() }
        }))
        .unwrap_or_default();
        format!("# Add to {}\n\n{}\n", target.display(), snippet)
    }

    fn describe_paths(&self, loc: Location) -> Vec<PathBuf> {
        vec![
            mcp_json_path(loc),
            settings_json_path(loc),
            instructions_path(loc),
        ]
    }
}

/// Per-file write helpers, exported so the legacy `config_writer.rs`
/// shim can call only the named operation (write_mcp_config writes ONLY
/// the MCP entry, etc.) instead of `ClaudeCodeTarget::install()` which
/// writes all three files. Without this split the shims silently
/// cause side effects callers don't expect.
pub fn write_mcp_entry(loc: Location) -> FileWrite {
    let file = mcp_json_path(loc);
    let mut existing = read_json_file(&file);
    let before = existing
        .get("mcpServers")
        .and_then(|m| m.get("codegraph"))
        .cloned();
    let after = get_mcp_server_config();

    if let Some(before_val) = &before {
        if json_deep_equal(before_val, &after) {
            // Already exactly what we'd write — preserve byte-identical file.
            return FileWrite {
                path: file,
                action: FileAction::Unchanged,
            };
        }
    }
    // 'created' here means: the file itself did not exist before this
    // write. A pre-existing MCP JSON file (`~/.claude.json` globally,
    // `./.mcp.json` locally) containing other MCP servers (no
    // `codegraph` key) is 'updated', not 'created' — we're adding an
    // entry to a file that was already there. Codex uses a different
    // idiom (empty-content => 'created') because its config.toml is
    // ours alone to manage.
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

/// Strip the codegraph entry from a legacy project-local
/// `./.claude.json` (written by pre-#207 installers, which Claude Code
/// never read). Surgical: only our `codegraph` key is removed; sibling
/// MCP servers and any unrelated keys are preserved, and the file is
/// deleted only when removal leaves it completely empty. Returns the
/// file action for reporting, or `None` when there's nothing to migrate.
fn cleanup_legacy_local_mcp() -> Option<FileWrite> {
    let file = legacy_local_mcp_path();
    if !file.exists() {
        return None;
    }
    let mut config = read_json_file(&file);
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
    if config.is_empty() {
        let _ = fs::remove_file(&file); // ignore
    } else {
        write_json_file(&file, &config);
    }
    Some(FileWrite {
        path: file,
        action: FileAction::Removed,
    })
}

/// True when a Claude Code hook `command` is one of the auto-sync hooks
/// a pre-0.8 install wrote. Those installers added
/// `PostToolUse(Edit|Write) → codegraph mark-dirty` and
/// `Stop → codegraph sync-if-dirty` (local builds used the
/// `npx @colbymchenry/codegraph …` form, which still contains the
/// `codegraph <subcommand>` substring). Both subcommands were later
/// removed from the CLI, so the Stop hook fails every turn with
/// "unknown command 'sync-if-dirty'". Matching on the codegraph-scoped
/// subcommand keeps unrelated user hooks (e.g. GitKraken's
/// `gk ai hook run`) untouched.
fn is_legacy_codegraph_hook_command(command: Option<&Value>) -> bool {
    match command.and_then(|c| c.as_str()) {
        Some(cmd) => {
            cmd.contains("codegraph mark-dirty") || cmd.contains("codegraph sync-if-dirty")
        }
        None => false,
    }
}

/// The front-load hook command installed under `UserPromptSubmit`.
/// Substring matching also recognizes the old npm wrapper form.
const PROMPT_HOOK_COMMAND: &str = "codegraph prompt-hook";

fn is_prompt_hook_command(command: Option<&Value>) -> bool {
    command
        .and_then(Value::as_str)
        .map(|cmd| cmd.contains(PROMPT_HOOK_COMMAND))
        .unwrap_or(false)
}

/// Remove selected hook commands from Claude `settings.json`.
///
/// Surgical at the individual-command level: only entries matching
/// `matches_command` are dropped, so sibling hooks in the same matcher group
/// survive. Empty groups/events are pruned only after a match is removed.
fn remove_hook_commands_matching<F>(loc: Location, matches_command: F) -> FileWrite
where
    F: Fn(Option<&Value>) -> bool,
{
    let file = settings_json_path(loc);
    if !file.exists() {
        return FileWrite {
            path: file,
            action: FileAction::NotFound,
        };
    }

    let mut settings = read_json_file(&file);
    let hooks_is_object = matches!(settings.get("hooks"), Some(Value::Object(_)));
    if !hooks_is_object {
        return FileWrite {
            path: file,
            action: FileAction::Unchanged,
        };
    }

    // Pass 1: drop the legacy command(s) from inside every matcher group.
    let mut removed_any = false;
    if let Some(Value::Object(hooks)) = settings.get_mut("hooks") {
        for (_event, groups) in hooks.iter_mut() {
            let groups = match groups {
                Value::Array(g) => g,
                _ => continue,
            };
            for group in groups.iter_mut() {
                let group_hooks = match group.get_mut("hooks") {
                    Some(Value::Array(h)) => h,
                    _ => continue,
                };
                let before = group_hooks.len();
                group_hooks.retain(|h| !matches_command(h.get("command")));
                if group_hooks.len() != before {
                    removed_any = true;
                }
            }
        }
    }

    if !removed_any {
        return FileWrite {
            path: file,
            action: FileAction::Unchanged,
        };
    }

    // Pass 2: prune empty matcher groups, then events with no groups
    // left, then an empty top-level `hooks`. Guarded by `removed_any` so
    // we never restructure a settings.json that had no codegraph hooks.
    if let Some(Value::Object(hooks)) = settings.get_mut("hooks") {
        let events: Vec<String> = hooks.keys().cloned().collect();
        for event in events {
            let empty = if let Some(Value::Array(groups)) = hooks.get_mut(&event) {
                groups.retain(|g| !matches!(g.get("hooks"), Some(Value::Array(h)) if h.is_empty()));
                groups.is_empty()
            } else {
                false
            };
            if empty {
                hooks.remove(&event);
            }
        }
        if hooks.is_empty() {
            settings.remove("hooks");
        }
    }

    write_json_file(&file, &settings);
    FileWrite {
        path: file,
        action: FileAction::Removed,
    }
}

/// Remove stale auto-sync hooks (`mark-dirty` / `sync-if-dirty`) written by
/// pre-0.8 installers. Used by both install self-healing and uninstall.
pub fn cleanup_legacy_hooks(loc: Location) -> FileWrite {
    remove_hook_commands_matching(loc, is_legacy_codegraph_hook_command)
}

/// Remove the front-load `UserPromptSubmit` hook written by this installer.
/// Used by uninstall and by an explicit installer opt-out.
pub fn remove_prompt_hook_entry(loc: Location) -> FileWrite {
    remove_hook_commands_matching(loc, is_prompt_hook_command)
}

pub fn write_permissions_entry(loc: Location) -> FileWrite {
    let file = settings_json_path(loc);
    let mut settings = read_json_file(&file);
    let created = !file.exists();

    if !matches!(settings.get("permissions"), Some(Value::Object(_))) {
        settings.insert("permissions".to_string(), Value::Object(Map::new()));
    }
    let mut changed_seen = false;
    if let Some(Value::Object(permissions)) = settings.get_mut("permissions") {
        if !matches!(permissions.get("allow"), Some(Value::Array(_))) {
            permissions.insert("allow".to_string(), Value::Array(Vec::new()));
        }
        if let Some(Value::Array(allow)) = permissions.get_mut("allow") {
            // TS computes `jsonDeepEqual(before, allow)`; since the loop
            // only ever appends, a length compare is equivalent.
            let before_len = allow.len();
            for perm in get_codegraph_permissions() {
                if !allow.iter().any(|p| p.as_str() == Some(perm)) {
                    allow.push(Value::String(perm.to_string()));
                }
            }
            changed_seen = allow.len() != before_len;
        }
    }
    if !changed_seen && !created {
        return FileWrite {
            path: file,
            action: FileAction::Unchanged,
        };
    }
    write_json_file(&file, &settings);
    FileWrite {
        path: file,
        action: if created {
            FileAction::Created
        } else {
            FileAction::Updated
        },
    }
}

/// Install the front-load `UserPromptSubmit` hook in Claude settings.
/// Existing settings and sibling hooks are preserved, and an existing
/// codegraph prompt hook makes this a byte-for-byte no-op.
pub fn write_prompt_hook_entry(loc: Location) -> FileWrite {
    let file = settings_json_path(loc);
    let created = !file.exists();
    let mut settings = read_json_file(&file);

    if !matches!(settings.get("hooks"), Some(Value::Object(_))) {
        settings.insert("hooks".to_string(), Value::Object(Map::new()));
    }
    if let Some(Value::Object(hooks)) = settings.get_mut("hooks") {
        if !matches!(hooks.get("UserPromptSubmit"), Some(Value::Array(_))) {
            hooks.insert("UserPromptSubmit".to_string(), Value::Array(Vec::new()));
        }

        if let Some(Value::Array(groups)) = hooks.get("UserPromptSubmit") {
            let already_present = groups.iter().any(|group| {
                group
                    .get("hooks")
                    .and_then(Value::as_array)
                    .map(|entries| {
                        entries
                            .iter()
                            .any(|entry| is_prompt_hook_command(entry.get("command")))
                    })
                    .unwrap_or(false)
            });
            if already_present {
                return FileWrite {
                    path: file,
                    action: FileAction::Unchanged,
                };
            }
        }

        if let Some(Value::Array(groups)) = hooks.get_mut("UserPromptSubmit") {
            groups.push(serde_json::json!({
                "hooks": [{ "type": "command", "command": PROMPT_HOOK_COMMAND }],
            }));
        }
    }

    write_json_file(&file, &settings);
    FileWrite {
        path: file,
        action: if created {
            FileAction::Created
        } else {
            FileAction::Updated
        },
    }
}

/// Strip only the marker-delimited CodeGraph block from CLAUDE.md, preserving
/// all user-owned content around it. Used by uninstall.
pub fn remove_instructions_entry(loc: Location) -> FileWrite {
    let file = instructions_path(loc);
    let action = remove_marked_section(&file, CODEGRAPH_SECTION_START, CODEGRAPH_SECTION_END);
    FileWrite { path: file, action }
}

pub static CLAUDE_TARGET: ClaudeCodeTarget = ClaudeCodeTarget;

//! Backwards-compat shim — original Claude-only writer functions.
//!
//! The installer now uses the multi-target architecture in
//! `./targets/`. This file is preserved so existing callers (the test
//! suite, downstream tooling) keep working unchanged. Each function
//! delegates to the Claude target. New code should import the target
//! registry from `targets/registry.rs` directly.
//!
//! Deprecated: use `targets/registry.rs` and the `AgentTarget`
//! abstraction instead.

use super::targets::claude::{write_mcp_entry, write_permissions_entry};
use super::targets::shared::{cwd, home_dir, is_truthy, read_json_file};
use super::targets::types::Location;

pub type InstallLocation = Location;

/// Each shim calls ONLY the named per-file helper — `write_mcp_config`
/// writes only the MCP JSON, `write_permissions` only settings.json. The
/// full multi-file install lives in `ClaudeCodeTarget::install()` which the
/// new orchestrator uses.
///
/// There is no `write_claude_md` shim anymore: codegraph stopped writing a
/// CLAUDE.md instructions block (issue #529) now that the MCP server's
/// `initialize` instructions are the single source of truth.
pub fn write_mcp_config(location: InstallLocation) {
    write_mcp_entry(location);
}

pub fn write_permissions(location: InstallLocation) {
    write_permissions_entry(location);
}

pub fn has_mcp_config(location: InstallLocation) -> bool {
    // local scope lives in ./.mcp.json (project scope); global is the
    // user-scope ~/.claude.json. Mirrors the Claude target's paths.
    let file = match location {
        Location::Global => home_dir().join(".claude.json"),
        Location::Local => cwd().join(".mcp.json"),
    };
    let config = read_json_file(&file);
    config
        .get("mcpServers")
        .and_then(|m| m.get("codegraph"))
        .map(is_truthy)
        .unwrap_or(false)
}

pub fn has_permissions(location: InstallLocation) -> bool {
    let file = match location {
        Location::Global => home_dir().join(".claude").join("settings.json"),
        Location::Local => cwd().join(".claude").join("settings.json"),
    };
    let settings = read_json_file(&file);
    match settings.get("permissions").and_then(|p| p.get("allow")) {
        Some(serde_json::Value::Array(allow)) => allow.iter().any(|p| {
            p.as_str()
                .map(|s| s.starts_with("mcp__codegraph__"))
                .unwrap_or(false)
        }),
        _ => false,
    }
}

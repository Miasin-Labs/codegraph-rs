//! Prime Agent target (Prime Intellect's coding agent).
//!
//! Prime Agent loads MCP servers from a `mcpServers` object in its
//! `settings.json`, resolved as:
//!
//!   - global: `~/.prime/agent/settings.json`
//!   - local:  `./.prime/agent/settings.json`
//!
//! (The location is overridable at runtime via the
//! `PRIME_AGENT_CODING_AGENT_DIR` / `PI_CODING_AGENT_DIR` env vars, but
//! the installer targets the on-disk default so the entry persists.)
//!
//! Each stdio entry is the standard shape the kernel's MCP client
//! reads — `{ "type": "stdio", "command", "args" }` — identical to
//! Claude / Cursor / Gemini, so we reuse `get_mcp_server_config()`.
//!
//! No external permissions/allowlist concept: Prime Agent gates tool
//! calls through its own runtime, so `InstallOptions` is a no-op here.

use std::fs;
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::shared::{
    cwd,
    get_mcp_server_config,
    home_dir,
    is_truthy,
    json_deep_equal,
    nonblank_env_path,
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

/// Resolve the Prime Agent config dir, honoring the same env overrides
/// the runtime uses. For `global`, an override wins over `~/.prime/agent`.
fn agent_dir(loc: Location) -> PathBuf {
    match loc {
        Location::Global => nonblank_env_path("PRIME_AGENT_CODING_AGENT_DIR")
            .or_else(|| nonblank_env_path("PI_CODING_AGENT_DIR"))
            .unwrap_or_else(|| home_dir().join(".prime").join("agent")),
        Location::Local => cwd().join(".prime").join("agent"),
    }
}

fn settings_json_path(loc: Location) -> PathBuf {
    agent_dir(loc).join("settings.json")
}

pub struct PrimeTarget;

impl AgentTarget for PrimeTarget {
    fn id(&self) -> TargetId {
        TargetId::Prime
    }

    fn display_name(&self) -> &'static str {
        "Prime Agent"
    }

    fn docs_url(&self) -> Option<&'static str> {
        Some("https://docs.primeintellect.ai/")
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
            Location::Global => agent_dir(Location::Global).exists() || file.exists(),
            Location::Local => file.exists() || agent_dir(Location::Local).exists(),
        };
        DetectionResult {
            installed,
            already_configured,
            config_path: Some(file),
        }
    }

    fn install(&self, loc: Location, _opts: &InstallOptions) -> WriteResult {
        WriteResult {
            files: vec![write_mcp_entry(loc)],
            notes: vec!["Restart Prime Agent (or reload MCP) to apply.".to_string()],
        }
    }

    fn uninstall(&self, loc: Location) -> WriteResult {
        let file = settings_json_path(loc);
        let mut config = read_json_file(&file);
        let has_codegraph = config
            .get("mcpServers")
            .and_then(|m| m.get("codegraph"))
            .map(is_truthy)
            .unwrap_or(false);
        let action = if has_codegraph {
            if let Some(Value::Object(servers)) = config.get_mut("mcpServers") {
                servers.remove("codegraph");
                if servers.is_empty() {
                    config.remove("mcpServers");
                }
            }
            write_json_file(&file, &config);
            FileAction::Removed
        } else {
            FileAction::NotFound
        };
        WriteResult {
            files: vec![FileWrite { path: file, action }],
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
        vec![settings_json_path(loc)]
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
    let action = if before.map(|b| is_truthy(&b)).unwrap_or(false) || file.exists() {
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

pub static PRIME_TARGET: PrimeTarget = PrimeTarget;

//! opencode target.
//!
//!   - MCP server entry to `~/.config/opencode/opencode.jsonc` (global,
//!     XDG-style; `%APPDATA%/opencode/opencode.jsonc` on Windows) or
//!     `./opencode.jsonc` (local). Falls back to `opencode.json` when a
//!     `.json` file already exists; defaults new installs to `.jsonc`
//!     because that's what opencode itself creates on first run.
//!   - Instructions to `~/.config/opencode/AGENTS.md` (global) or
//!     `./AGENTS.md` (local). opencode reads AGENTS.md for agent
//!     instructions — same convention Codex CLI uses.
//!   - No permissions concept.
//!
//! Config shape uses opencode's wrapper:
//! ```jsonc
//! {
//!   "$schema": "https://opencode.ai/config.json",
//!   "mcp": { "codegraph": { "type": "local", "command": [...], "enabled": true } }
//! }
//! ```
//!
//! The shape differs from Claude/Cursor — opencode uses `mcp.<name>`
//! (not `mcpServers`), takes `command` as a string array combining
//! binary + args, and includes an explicit `enabled` flag.
//!
//! Reads + writes go through `jsonc-parser`'s CST so any `//` and
//! `/* */` comments the user has added to their `.jsonc` survive
//! idempotent re-runs.

use std::env;
use std::path::PathBuf;

use jsonc_parser::ParseOptions;
use jsonc_parser::cst::{CstInputValue, CstRootNode};
use serde_json::{Map, Value, json};

use super::shared::{
    atomic_write_file_sync,
    cwd,
    home_dir,
    is_truthy,
    json_deep_equal,
    remove_marked_section,
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

fn global_config_dir() -> PathBuf {
    if cfg!(windows) {
        let app_data = env::var_os("APPDATA")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join("AppData").join("Roaming"));
        return app_data.join("opencode");
    }
    // XDG_CONFIG_HOME if set, else ~/.config — matches opencode's docs.
    let xdg = env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config"));
    xdg.join("opencode")
}

fn config_base_dir(loc: Location) -> PathBuf {
    match loc {
        Location::Global => global_config_dir(),
        Location::Local => cwd(),
    }
}

// Pick existing .jsonc, then .json, default to .jsonc for new files.
// opencode auto-creates .jsonc on first run, so that's the dominant
// real-world case and the sensible default for greenfield installs.
fn config_path(loc: Location) -> PathBuf {
    let dir = config_base_dir(loc);
    let jsonc = dir.join("opencode.jsonc");
    let json = dir.join("opencode.json");
    if jsonc.exists() {
        return jsonc;
    }
    if json.exists() {
        return json;
    }
    jsonc
}

fn instructions_path(loc: Location) -> PathBuf {
    config_base_dir(loc).join("AGENTS.md")
}

fn read_config_text(file: &PathBuf) -> String {
    if !file.exists() {
        return String::new();
    }
    std::fs::read_to_string(file).unwrap_or_default()
}

/// Lenient JSONC parse → serde map; `{}` on anything unusable. Mirrors
/// the TS `parse(text, errors, { allowTrailingComma: true })` reader.
fn parse_config(text: &str) -> Map<String, Value> {
    if text.trim().is_empty() {
        return Map::new();
    }
    match jsonc_parser::parse_to_value(text, &ParseOptions::default()) {
        Ok(Some(value)) => match jsonc_value_to_serde(value) {
            Value::Object(map) => map,
            _ => Map::new(),
        },
        _ => Map::new(),
    }
}

/// Convert jsonc-parser's `JsonValue` to a `serde_json::Value` (the
/// crate's own `serde_json` feature isn't enabled, so we bridge by hand).
fn jsonc_value_to_serde(value: jsonc_parser::JsonValue<'_>) -> Value {
    use jsonc_parser::JsonValue as J;
    match value {
        J::Null => Value::Null,
        J::Boolean(b) => Value::Bool(b),
        J::Number(n) => n
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        J::String(s) => Value::String(s.into_owned()),
        J::Array(arr) => Value::Array(
            arr.take_inner()
                .into_iter()
                .map(jsonc_value_to_serde)
                .collect(),
        ),
        J::Object(obj) => {
            let mut map = Map::new();
            for (k, v) in obj {
                map.insert(k, jsonc_value_to_serde(v));
            }
            Value::Object(map)
        }
    }
}

fn get_opencode_server_entry() -> Value {
    json!({
        "type": "local",
        "command": ["codegraph", "serve", "--mcp"],
        "enabled": true,
    })
}

fn opencode_entry_input() -> CstInputValue {
    CstInputValue::Object(vec![
        (
            "type".to_string(),
            CstInputValue::String("local".to_string()),
        ),
        (
            "command".to_string(),
            CstInputValue::Array(vec![
                CstInputValue::String("codegraph".to_string()),
                CstInputValue::String("serve".to_string()),
                CstInputValue::String("--mcp".to_string()),
            ]),
        ),
        ("enabled".to_string(), CstInputValue::Bool(true)),
    ])
}

pub struct OpencodeTarget;

impl AgentTarget for OpencodeTarget {
    fn id(&self) -> TargetId {
        TargetId::Opencode
    }

    fn display_name(&self) -> &'static str {
        "opencode"
    }

    fn docs_url(&self) -> Option<&'static str> {
        Some("https://opencode.ai/docs/config")
    }

    fn supports_location(&self, _loc: Location) -> bool {
        true
    }

    fn detect(&self, loc: Location) -> DetectionResult {
        let file = config_path(loc);
        let config = parse_config(&read_config_text(&file));
        let already_configured = config
            .get("mcp")
            .and_then(|m| m.get("codegraph"))
            .map(is_truthy)
            .unwrap_or(false);
        let installed = match loc {
            Location::Global => global_config_dir().exists(),
            Location::Local => file.exists(),
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

        // AGENTS.md is no longer written — the codegraph usage guidance
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
        let file = config_path(loc);

        if !file.exists() {
            files.push(FileWrite {
                path: file,
                action: FileAction::NotFound,
            });
        } else {
            let text = read_config_text(&file);
            let config = parse_config(&text);
            let has_codegraph = config
                .get("mcp")
                .and_then(|m| m.get("codegraph"))
                .map(is_truthy)
                .unwrap_or(false);
            if !has_codegraph {
                files.push(FileWrite {
                    path: file,
                    action: FileAction::NotFound,
                });
            } else if let Ok(root) = CstRootNode::parse(&text, &ParseOptions::default()) {
                // Drop our key surgically. Leaves siblings + comments untouched.
                if let Some(obj) = root.object_value() {
                    if let Some(mcp_prop) = obj.get("mcp") {
                        if let Some(mcp_obj) = mcp_prop.object_value() {
                            if let Some(cg) = mcp_obj.get("codegraph") {
                                cg.remove();
                            }
                            // If `mcp` is now an empty object, drop the wrapper too.
                            if mcp_obj.properties().is_empty() {
                                if let Some(mcp_prop) = obj.get("mcp") {
                                    mcp_prop.remove();
                                }
                            }
                        }
                    }
                }
                let updated = root.to_string();
                let _ = atomic_write_file_sync(&file, &updated);
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
        }

        files.push(remove_instructions_entry(loc));

        WriteResult {
            files,
            notes: Vec::new(),
        }
    }

    fn print_config(&self, loc: Location) -> String {
        let target = config_path(loc);
        let snippet = serde_json::to_string_pretty(&json!({
            "$schema": "https://opencode.ai/config.json",
            "mcp": { "codegraph": get_opencode_server_entry() },
        }))
        .unwrap_or_default();
        format!("# Add to {}\n\n{}\n", target.display(), snippet)
    }

    fn describe_paths(&self, loc: Location) -> Vec<PathBuf> {
        vec![config_path(loc), instructions_path(loc)]
    }
}

fn write_mcp_entry(loc: Location) -> FileWrite {
    let file = config_path(loc);
    let existed = file.exists();
    let mut text = read_config_text(&file);

    // Seed a minimal opencode config when the file is brand-new so
    // the result is a complete, schema-tagged file (not just a bare
    // `{ "mcp": {...} }`).
    if text.trim().is_empty() {
        text = "{\n  \"$schema\": \"https://opencode.ai/config.json\"\n}\n".to_string();
    }

    let config = parse_config(&text);
    let before = config.get("mcp").and_then(|m| m.get("codegraph"));
    let after = get_opencode_server_entry();

    if let Some(before_val) = before {
        if json_deep_equal(before_val, &after) {
            return FileWrite {
                path: file,
                action: FileAction::Unchanged,
            };
        }
    }

    let root = match CstRootNode::parse(&text, &ParseOptions::default()) {
        Ok(r) => r,
        Err(_) => {
            // Unparseable JSONC — rebuild from the minimal seed rather
            // than corrupting the file further (the TS jsonc-parser
            // `modify` path is similarly defensive).
            let seed = "{\n  \"$schema\": \"https://opencode.ai/config.json\"\n}\n";
            CstRootNode::parse(seed, &ParseOptions::default()).expect("seed config is valid JSONC")
        }
    };

    let obj = root.object_value_or_set();

    // Add $schema if the user's existing file is missing it.
    let schema_truthy = config.get("$schema").map(is_truthy).unwrap_or(false);
    if !schema_truthy {
        match obj.get("$schema") {
            Some(prop) => prop.set_value(CstInputValue::String(
                "https://opencode.ai/config.json".to_string(),
            )),
            None => {
                obj.append(
                    "$schema",
                    CstInputValue::String("https://opencode.ai/config.json".to_string()),
                );
            }
        }
    }

    // Surgical edit — preserves comments, formatting, and order of
    // every key we don't touch.
    let mcp = obj.object_value_or_set("mcp");
    match mcp.get("codegraph") {
        Some(prop) => prop.set_value(opencode_entry_input()),
        None => {
            mcp.append("codegraph", opencode_entry_input());
        }
    }
    let updated = root.to_string();
    let _ = atomic_write_file_sync(&file, &updated);

    FileWrite {
        path: file,
        action: if existed {
            FileAction::Updated
        } else {
            FileAction::Created
        },
    }
}

/// Strip the marker-delimited CodeGraph block from AGENTS.md if a prior
/// install wrote one. Used by both install (self-heal on upgrade) and
/// uninstall — see issue #529.
fn remove_instructions_entry(loc: Location) -> FileWrite {
    let file = instructions_path(loc);
    let action = remove_marked_section(&file, CODEGRAPH_SECTION_START, CODEGRAPH_SECTION_END);
    FileWrite { path: file, action }
}

pub static OPENCODE_TARGET: OpencodeTarget = OpencodeTarget;

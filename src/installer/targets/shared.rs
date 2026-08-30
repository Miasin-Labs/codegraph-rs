//! Helpers shared across `AgentTarget` implementations.
//!
//! Lifted from the original `config-writer.ts` so each target can
//! compose them without inheritance. Kept deliberately small — the
//! targets are different enough (JSON vs TOML vs Markdown, varying
//! idempotency markers) that a base class would force the awkward
//! shape onto everyone.

use std::path::{Path, PathBuf};
use std::{env, fs};

use jsonc_parser::ParseOptions;
use jsonc_parser::cst::{CstInputValue, CstRootNode};
use serde_json::{Map, Value, json};

use super::types::{FileAction, FileWrite};
use crate::installer::instructions_template::{
    CODEGRAPH_INSTRUCTIONS_BLOCK,
    CODEGRAPH_SECTION_END,
    CODEGRAPH_SECTION_START,
};

/// Node `os.homedir()` parity: `$HOME` first on POSIX (`%USERPROFILE%`
/// on Windows), falling back to the OS account database. The test
/// suite redirects home via these env vars, same as the TS suite.
pub(crate) fn home_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(p) = env::var_os("USERPROFILE") {
            if !p.is_empty() {
                return PathBuf::from(p);
            }
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(p) = env::var_os("HOME") {
            if !p.is_empty() {
                return PathBuf::from(p);
            }
        }
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// Node `process.cwd()` parity.
pub(crate) fn cwd() -> PathBuf {
    env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub(crate) fn nonblank_env_path(name: &str) -> Option<PathBuf> {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
}

/// JS truthiness for a JSON value (`!!config.mcpServers?.codegraph`).
pub(crate) fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// The MCP-server config block codegraph injects. Same shape across
/// all JSON-shaped agent configs (Claude, Cursor, opencode), only the
/// surrounding wrapper differs. Codex (TOML) builds its own block.
pub fn get_mcp_server_config() -> Value {
    json!({
        "type": "stdio",
        "command": "codegraph",
        "args": ["serve", "--mcp"],
    })
}

/// Permissions list for Claude `settings.json`. Other targets that
/// have a permissions concept can compose this list directly. The
/// permission strings follow Claude's `mcp__<server>__<tool>` format.
pub fn get_codegraph_permissions() -> Vec<&'static str> {
    vec![
        "mcp__codegraph__codegraph_explore",
        "mcp__codegraph__codegraph_search",
        "mcp__codegraph__codegraph_node",
        "mcp__codegraph__codegraph_callers",
        "mcp__codegraph__codegraph_callees",
        "mcp__codegraph__codegraph_impact",
        "mcp__codegraph__codegraph_files",
        "mcp__codegraph__codegraph_status",
    ]
}

/// Read a JSON file, returning `{}` when missing or unparseable.
///
/// Unparseable files are backed up to `<path>.backup` BEFORE we return
/// `{}` — so an idempotent re-run never silently deletes a user's
/// existing config that happened to break JSON parse temporarily.
pub fn read_json_file(file_path: &Path) -> Map<String, Value> {
    if !file_path.exists() {
        return Map::new();
    }
    let text = match fs::read_to_string(file_path) {
        Ok(t) => t,
        Err(err) => {
            warn_unparseable(file_path, &err.to_string());
            return Map::new();
        }
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => map,
        // Valid JSON that isn't an object — treat as empty config (the
        // TS code would misbehave here too; nothing downstream supports
        // a non-object root).
        Ok(_) => Map::new(),
        Err(err) => {
            warn_unparseable(file_path, &err.to_string());
            Map::new()
        }
    }
}

fn warn_unparseable(file_path: &Path, msg: &str) {
    let basename = file_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| file_path.display().to_string());
    eprintln!("  Warning: Could not parse {basename}: {msg}");
    eprintln!("  A backup will be created before overwriting.");
    let backup = PathBuf::from(format!("{}.backup", file_path.display()));
    let _ = fs::copy(file_path, backup); // ignore backup failure
}

/// Write a file atomically: write to `<path>.tmp.<pid>`, then rename.
///
/// Prevents corruption if the process crashes mid-write. The temp
/// file is cleaned up on rename failure.
pub fn atomic_write_file_sync(file_path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(dir) = file_path.parent() {
        if !dir.exists() {
            fs::create_dir_all(dir)?;
        }
    }
    let tmp_path = PathBuf::from(format!(
        "{}.tmp.{}",
        file_path.display(),
        std::process::id()
    ));
    let result = fs::write(&tmp_path, content).and_then(|_| fs::rename(&tmp_path, file_path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp_path); // ignore
    }
    result
}

/// Atomic JSON write. Trailing newline matches the convention every
/// existing target had — preserves diff-friendly file shape.
/// 2-space indent — `JSON.stringify(data, null, 2)` parity.
pub fn write_json_file(file_path: &Path, data: &Map<String, Value>) {
    let body = serde_json::to_string_pretty(&Value::Object(data.clone()))
        .unwrap_or_else(|_| "{}".to_string());
    let _ = atomic_write_file_sync(file_path, &format!("{body}\n"));
}

/// Compare two JSON values for deep equality, ignoring key order.
///
/// Used for idempotency: when the on-disk config already exactly
/// matches what we'd write, return action=`unchanged` instead of
/// re-writing (and emitting a confusing "Updated" log line).
pub fn json_deep_equal(a: &Value, b: &Value) -> bool {
    // serde_json's PartialEq is structural and key-order-independent.
    a == b
}

pub(crate) fn read_jsonc_file(file_path: &Path) -> String {
    fs::read_to_string(file_path).unwrap_or_default()
}

pub(crate) fn parse_jsonc_object(text: &str) -> Map<String, Value> {
    if text.trim().is_empty() {
        return Map::new();
    }
    match jsonc_parser::parse_to_value(text, &ParseOptions::default()) {
        Ok(Some(jsonc_parser::JsonValue::Object(object))) => object
            .into_iter()
            .map(|(key, value)| (key, jsonc_to_serde(value)))
            .collect(),
        _ => Map::new(),
    }
}

fn jsonc_to_serde(value: jsonc_parser::JsonValue<'_>) -> Value {
    use jsonc_parser::JsonValue as Jsonc;
    match value {
        Jsonc::Null => Value::Null,
        Jsonc::Boolean(value) => Value::Bool(value),
        Jsonc::Number(value) => value
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Jsonc::String(value) => Value::String(value.into_owned()),
        Jsonc::Array(values) => Value::Array(
            values
                .take_inner()
                .into_iter()
                .map(jsonc_to_serde)
                .collect(),
        ),
        Jsonc::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, jsonc_to_serde(value)))
                .collect(),
        ),
    }
}

pub(crate) fn upsert_jsonc_entry(
    file_path: &Path,
    wrapper: &str,
    key: &str,
    after: &Value,
    input: CstInputValue,
) -> FileWrite {
    let existed = file_path.exists();
    let mut text = read_jsonc_file(file_path);
    if text.trim().is_empty() {
        text = "{}\n".to_string();
    }
    let config = parse_jsonc_object(&text);
    if config
        .get(wrapper)
        .and_then(|value| value.get(key))
        .is_some_and(|before| json_deep_equal(before, after))
    {
        return FileWrite {
            path: file_path.to_path_buf(),
            action: FileAction::Unchanged,
        };
    }

    let root = CstRootNode::parse(&text, &ParseOptions::default()).unwrap_or_else(|_| {
        CstRootNode::parse("{}\n", &ParseOptions::default())
            .expect("static empty JSONC object is valid")
    });
    let object = root.object_value_or_set();
    let entries = object.object_value_or_set(wrapper);
    match entries.get(key) {
        Some(property) => property.set_value(input),
        None => {
            entries.append(key, input);
        }
    }
    let _ = atomic_write_file_sync(file_path, &root.to_string());
    FileWrite {
        path: file_path.to_path_buf(),
        action: if existed {
            FileAction::Updated
        } else {
            FileAction::Created
        },
    }
}

pub(crate) fn remove_jsonc_entry(file_path: &Path, wrapper: &str, key: &str) -> FileWrite {
    if !file_path.exists() {
        return FileWrite {
            path: file_path.to_path_buf(),
            action: FileAction::NotFound,
        };
    }
    let text = read_jsonc_file(file_path);
    let config = parse_jsonc_object(&text);
    if !config
        .get(wrapper)
        .and_then(|value| value.get(key))
        .map(is_truthy)
        .unwrap_or(false)
    {
        return FileWrite {
            path: file_path.to_path_buf(),
            action: FileAction::NotFound,
        };
    }
    let root = match CstRootNode::parse(&text, &ParseOptions::default()) {
        Ok(root) => root,
        Err(_) => {
            return FileWrite {
                path: file_path.to_path_buf(),
                action: FileAction::NotFound,
            };
        }
    };
    if let Some(object) = root.object_value() {
        if let Some(entries) = object
            .get(wrapper)
            .and_then(|property| property.object_value())
        {
            if let Some(property) = entries.get(key) {
                property.remove();
            }
            if entries.properties().is_empty() {
                if let Some(property) = object.get(wrapper) {
                    property.remove();
                }
            }
        }
    }
    let _ = atomic_write_file_sync(file_path, &root.to_string());
    FileWrite {
        path: file_path.to_path_buf(),
        action: FileAction::Removed,
    }
}

/// Action returned by [`replace_or_append_marked_section`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkedSectionAction {
    Created,
    Updated,
    Appended,
    Unchanged,
}

/// Replace or append a marker-delimited section in a markdown-ish file.
///
/// Used for the `<!-- CODEGRAPH_START --> ... <!-- CODEGRAPH_END -->`
/// block. Preserves all content outside the markers verbatim.
///
/// Returns `Created` when the file didn't exist; `Updated` when
/// markers were found and content swapped; `Appended` when markers
/// weren't found and section was added at end. `Unchanged` when the
/// existing block already matches `body`.
pub fn replace_or_append_marked_section(
    file_path: &Path,
    body: &str,
    start_marker: &str,
    end_marker: &str,
) -> MarkedSectionAction {
    if !file_path.exists() {
        let _ = atomic_write_file_sync(file_path, &format!("{body}\n"));
        return MarkedSectionAction::Created;
    }

    let content = fs::read_to_string(file_path).unwrap_or_default();
    let start_idx = content.find(start_marker);
    let end_idx = content.find(end_marker);

    if let (Some(start_idx), Some(end_idx)) = (start_idx, end_idx) {
        if end_idx > start_idx {
            let existing_block = &content[start_idx..end_idx + end_marker.len()];
            if existing_block == body {
                return MarkedSectionAction::Unchanged;
            }
            let before = &content[..start_idx];
            let after = &content[end_idx + end_marker.len()..];
            let _ = atomic_write_file_sync(file_path, &format!("{before}{body}{after}"));
            return MarkedSectionAction::Updated;
        }
    }

    // No markers — append. Preserve existing content with a separating
    // blank line.
    let trimmed = content.trim_end();
    let sep = if !trimmed.is_empty() { "\n\n" } else { "" };
    let _ = atomic_write_file_sync(file_path, &format!("{trimmed}{sep}{body}\n"));
    MarkedSectionAction::Appended
}

/// Upsert the current short CodeGraph block into an agent instructions file.
/// Existing content outside the marker fence is preserved verbatim, stale
/// fenced blocks are replaced, and byte-identical re-runs do not rewrite.
pub fn upsert_instructions_entry(file_path: &Path) -> FileWrite {
    let action = replace_or_append_marked_section(
        file_path,
        CODEGRAPH_INSTRUCTIONS_BLOCK,
        CODEGRAPH_SECTION_START,
        CODEGRAPH_SECTION_END,
    );
    let action = match action {
        MarkedSectionAction::Created => FileAction::Created,
        MarkedSectionAction::Updated | MarkedSectionAction::Appended => FileAction::Updated,
        MarkedSectionAction::Unchanged => FileAction::Unchanged,
    };
    FileWrite {
        path: file_path.to_path_buf(),
        action,
    }
}

/// Inverse of [`replace_or_append_marked_section`]. Strips the marker
/// block from `file_path` if present. If the file becomes empty after
/// removal, deletes the file entirely (matches the existing Claude
/// uninstall behavior).
///
/// Returns `Removed` when content was stripped, `NotFound` when
/// the markers weren't present, `Kept` when the file didn't exist.
pub fn remove_marked_section(file_path: &Path, start_marker: &str, end_marker: &str) -> FileAction {
    if !file_path.exists() {
        return FileAction::Kept;
    }

    let content = match fs::read_to_string(file_path) {
        Ok(c) => c,
        Err(_) => return FileAction::Kept,
    };

    let start_idx = match content.find(start_marker) {
        Some(i) => i,
        None => return FileAction::NotFound,
    };
    let end_idx = match content.find(end_marker) {
        Some(i) => i,
        None => return FileAction::NotFound,
    };
    if end_idx <= start_idx {
        return FileAction::NotFound;
    }

    let before = content[..start_idx].trim_end();
    let after = content[end_idx + end_marker.len()..].trim_start();
    let sep = if !before.is_empty() && !after.is_empty() {
        "\n\n"
    } else {
        ""
    };
    let joined = format!("{before}{sep}{after}");

    if joined.trim().is_empty() {
        let _ = fs::remove_file(file_path); // ignore
    } else {
        let _ = atomic_write_file_sync(file_path, &format!("{}\n", joined.trim()));
    }
    FileAction::Removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_deep_equal_ignores_key_order() {
        let a: Value = serde_json::from_str(r#"{"a":1,"b":{"c":[1,2]}}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"b":{"c":[1,2]},"a":1}"#).unwrap();
        assert!(json_deep_equal(&a, &b));
        let c: Value = serde_json::from_str(r#"{"a":1,"b":{"c":[2,1]}}"#).unwrap();
        assert!(!json_deep_equal(&a, &c));
    }

    #[test]
    fn mcp_server_config_shape() {
        let cfg = get_mcp_server_config();
        assert_eq!(cfg["type"], "stdio");
        assert_eq!(cfg["command"], "codegraph");
        assert_eq!(cfg["args"], json!(["serve", "--mcp"]));
    }
}

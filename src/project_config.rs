//! Project-scoped `codegraph.json` configuration.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::str::FromStr;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde_json::Value;

use crate::error::log_warn;
use crate::types::Language;

pub const PROJECT_CONFIG_FILENAME: &str = "codegraph.json";

#[derive(Clone)]
struct CacheEntry {
    modified: Option<SystemTime>,
    len: u64,
    config: ProjectConfig,
}

static CONFIG_CACHE: OnceLock<Mutex<HashMap<std::path::PathBuf, CacheEntry>>> = OnceLock::new();

/// Validated project configuration. Missing and malformed values are ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectConfig {
    /// Custom `.extension` to language mappings. These override built-ins.
    pub extensions: HashMap<String, Language>,
    /// Gitignored directories containing embedded repositories to opt back in.
    pub include_ignored: Vec<String>,
    /// Project-relative gitignore-style patterns to exclude, including tracked files.
    pub exclude: Vec<String>,
    /// Project-relative gitignore-style patterns to include despite `.gitignore`.
    pub include: Vec<String>,
}

impl ProjectConfig {
    pub fn load(project_root: &Path) -> Self {
        load_project_config(project_root)
    }

    pub fn extension_overrides(&self) -> &HashMap<String, Language> {
        &self.extensions
    }

    pub(crate) fn exclude_matcher(&self, root: &Path) -> Option<Gitignore> {
        build_pattern_matcher(root, &self.exclude, "exclude")
    }

    pub(crate) fn include_matcher(&self, root: &Path) -> Option<Gitignore> {
        build_pattern_matcher(root, &self.include, "include")
    }

    pub(crate) fn include_ignored_matcher(&self, root: &Path) -> Option<Gitignore> {
        build_pattern_matcher(root, &self.include_ignored, "includeIgnored")
    }
}

/// Load and validate `<project>/codegraph.json`.
///
/// Every failure is non-fatal. A missing/unreadable file or invalid top-level
/// JSON yields the zero-config default; invalid individual entries are skipped.
pub fn load_project_config(project_root: &Path) -> ProjectConfig {
    let file = project_root.join(PROJECT_CONFIG_FILENAME);
    let metadata = match fs::metadata(&file) {
        Ok(metadata) => metadata,
        Err(_) => {
            if let Some(cache) = CONFIG_CACHE.get() {
                cache
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(project_root);
            }
            return ProjectConfig::default();
        }
    };
    let modified = metadata.modified().ok();
    let len = metadata.len();
    let cache = CONFIG_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(entry) = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(project_root)
        .filter(|entry| entry.modified == modified && entry.len == len)
        .cloned()
    {
        return entry.config;
    }

    let raw = match fs::read_to_string(&file) {
        Ok(raw) => raw,
        Err(_) => return ProjectConfig::default(),
    };
    let parsed: Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(error) => {
            log_warn(
                &format!("Ignoring {PROJECT_CONFIG_FILENAME}: not valid JSON"),
                Some(&serde_json::json!({
                    "file": file.to_string_lossy(),
                    "error": error.to_string(),
                })),
            );
            return ProjectConfig::default();
        }
    };
    let Some(object) = parsed.as_object() else {
        return ProjectConfig::default();
    };

    let config = ProjectConfig {
        extensions: extract_extensions(object.get("extensions"), &file),
        include_ignored: extract_patterns(object.get("includeIgnored"), "includeIgnored", &file),
        exclude: extract_patterns(object.get("exclude"), "exclude", &file),
        include: extract_patterns(object.get("include"), "include", &file),
    };
    cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(
            project_root.to_path_buf(),
            CacheEntry {
                modified,
                len,
                config: config.clone(),
            },
        );
    config
}

/// Forget cached project configs. Primarily useful after programmatic rewrites.
pub fn clear_project_config_cache() {
    if let Some(cache) = CONFIG_CACHE.get() {
        cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }
}

fn extract_extensions(value: Option<&Value>, file: &Path) -> HashMap<String, Language> {
    let Some(value) = value else {
        return HashMap::new();
    };
    let Some(entries) = value.as_object() else {
        warn_config(file, "Ignoring \"extensions\": must be an object");
        return HashMap::new();
    };

    let mut out = HashMap::new();
    for (raw_key, raw_value) in entries {
        let Some(key) = normalize_extension(raw_key) else {
            warn_config(
                file,
                &format!("Ignoring extension mapping \"{raw_key}\": invalid file extension"),
            );
            continue;
        };
        let Some(raw_language) = raw_value.as_str() else {
            warn_config(
                file,
                &format!("Ignoring extension \"{raw_key}\": language must be a string"),
            );
            continue;
        };
        let language = match Language::from_str(raw_language) {
            Ok(language) if language != Language::Unknown => language,
            _ => {
                warn_config(
                    file,
                    &format!(
                        "Ignoring extension \"{raw_key}\": \"{raw_language}\" is not a supported language"
                    ),
                );
                continue;
            }
        };
        out.insert(key, language);
    }
    out
}

fn extract_patterns(value: Option<&Value>, field: &str, file: &Path) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    let Some(entries) = value.as_array() else {
        warn_config(
            file,
            &format!("Ignoring \"{field}\": must be an array of gitignore-style patterns"),
        );
        return Vec::new();
    };

    let mut out = Vec::new();
    for entry in entries {
        match entry.as_str().map(str::trim) {
            Some(pattern) if !pattern.is_empty() => out.push(pattern.to_string()),
            _ => warn_config(
                file,
                &format!("Ignoring a \"{field}\" entry: every pattern must be a non-empty string"),
            ),
        }
    }
    out
}

fn normalize_extension(raw: &str) -> Option<String> {
    let mut extension = raw.trim().to_lowercase();
    if extension.is_empty() {
        return None;
    }
    if !extension.starts_with('.') {
        extension.insert(0, '.');
    }
    let body = &extension[1..];
    if body.is_empty() || body.contains('.') || body.contains('/') || body.contains('\\') {
        return None;
    }
    Some(extension)
}

pub(crate) fn build_pattern_matcher(
    root: &Path,
    patterns: &[String],
    field: &str,
) -> Option<Gitignore> {
    if patterns.is_empty() {
        return None;
    }
    let mut builder = GitignoreBuilder::new(root);
    let mut valid = 0usize;
    for pattern in patterns {
        match builder.add_line(None, pattern) {
            Ok(_) => valid += 1,
            Err(error) => log_warn(
                &format!("Ignoring invalid \"{field}\" pattern in {PROJECT_CONFIG_FILENAME}"),
                Some(&serde_json::json!({
                    "pattern": pattern,
                    "error": error.to_string(),
                })),
            ),
        }
    }
    if valid == 0 {
        return None;
    }
    builder.build().ok()
}

pub(crate) fn matcher_matches(matcher: Option<&Gitignore>, path: &str, is_dir: bool) -> bool {
    matcher
        .map(|matcher| {
            matcher
                .matched_path_or_any_parents(path, is_dir)
                .is_ignore()
        })
        .unwrap_or(false)
}

fn warn_config(file: &Path, message: &str) {
    log_warn(
        &format!("{message} in {PROJECT_CONFIG_FILENAME}"),
        Some(&serde_json::json!({ "file": file.to_string_lossy() })),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_and_normalizes_all_supported_fields() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join(PROJECT_CONFIG_FILENAME),
            r#"{
                "extensions": { "DOTA_LUA": "lua", ".tpl": "php" },
                "includeIgnored": ["repos/"],
                "exclude": ["vendor/**"],
                "include": ["Tools/**"]
            }"#,
        )
        .unwrap();

        let config = load_project_config(temp.path());
        assert_eq!(config.extensions.get(".dota_lua"), Some(&Language::Lua));
        assert_eq!(config.extensions.get(".tpl"), Some(&Language::Php));
        assert_eq!(config.include_ignored, ["repos/"]);
        assert_eq!(config.exclude, ["vendor/**"]);
        assert_eq!(config.include, ["Tools/**"]);
    }

    #[test]
    fn malformed_and_invalid_entries_degrade_to_defaults() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join(PROJECT_CONFIG_FILENAME),
            r#"{
                "extensions": { ".d.ts": "typescript", ".ok": "lua", ".bad": "wat" },
                "includeIgnored": ["", 7],
                "exclude": "vendor/",
                "include": ["src/**"]
            }"#,
        )
        .unwrap();

        let config = load_project_config(temp.path());
        assert_eq!(config.extensions.len(), 1);
        assert_eq!(config.extensions.get(".ok"), Some(&Language::Lua));
        assert!(config.include_ignored.is_empty());
        assert!(config.exclude.is_empty());
        assert_eq!(config.include, ["src/**"]);

        fs::write(temp.path().join(PROJECT_CONFIG_FILENAME), "not json").unwrap();
        assert_eq!(load_project_config(temp.path()), ProjectConfig::default());
    }
}

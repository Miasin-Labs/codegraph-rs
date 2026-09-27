//! A project's own rules: `.codegraph/rules/<id>.yaml`, written by
//! `save` (the MCP `rules` tool) once a rule's examples pass, and run by
//! every `analyze rules` sweep unless `--no-saved` — so a rule written for
//! one bug keeps looking for its variants.

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::check::{CheckReport, check_rules};
use super::compile::RuleSet;
use crate::directory::get_codegraph_dir;

/// Where a project keeps its rules.
pub fn rules_dir(project_root: &Path) -> PathBuf {
    get_codegraph_dir(project_root).join("rules")
}

/// The `(label, yaml)` of every saved rule file, in path order.
pub fn saved_rule_texts(project_root: &Path) -> Vec<(String, String)> {
    let dir = rules_dir(project_root);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext == "yaml" || ext == "yml")
        })
        .collect();
    files.sort();
    files
        .into_iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            Some((path.display().to_string(), text))
        })
        .collect()
}

impl RuleSet {
    /// Add the project's saved rules, except those whose id is already
    /// loaded (see [`RuleSet::add_shadowed`]).
    pub fn add_saved(&mut self, project_root: &Path) -> usize {
        self.add_shadowed(&saved_rule_texts(project_root))
    }
}

/// What `save` did.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveOutcome {
    pub id: String,
    /// Project-relative path of the file written.
    pub path: String,
    /// A saved rule of this id was replaced.
    pub replaced: bool,
}

/// Why `save` refused.
#[derive(Debug)]
pub enum SaveError {
    /// The YAML did not hold exactly one rule, or the id is not a file name.
    Invalid(String),
    /// The rule's examples did not pass (the check report says why).
    Check(CheckReport),
    Io(String),
}

/// Check the one rule in `yaml` and, when every example passes, write it to
/// `.codegraph/rules/<id>.yaml`.
pub fn save_rule(project_root: &Path, yaml: &str) -> Result<SaveOutcome, SaveError> {
    let mut rules = RuleSet::default();
    rules.add_text("<rule>", yaml);
    let report = check_rules(&rules);
    if !report.ok() {
        return Err(SaveError::Check(report));
    }
    let [rule] = rules.rules.as_slice() else {
        return Err(SaveError::Invalid(format!(
            "save takes one rule; the YAML holds {}",
            rules.rules.len()
        )));
    };
    let id = rule.id.as_str();
    if id.is_empty()
        || id.starts_with('.')
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(SaveError::Invalid(format!(
            "rule id `{id}` is not usable as a file name (letters, digits, `-`, `_`, `.`)"
        )));
    }
    if rule.examples.iter().all(|example| !example.bad) {
        return Err(SaveError::Invalid(
            "a saved rule needs at least one `bad` example (the bug it finds)".to_string(),
        ));
    }
    let dir = rules_dir(project_root);
    std::fs::create_dir_all(&dir).map_err(|e| SaveError::Io(e.to_string()))?;
    let path = dir.join(format!("{id}.yaml"));
    let replaced = path.exists();
    let mut text = yaml.trim_end().to_string();
    text.push('\n');
    let tmp = dir.join(format!(".{id}.yaml.tmp"));
    std::fs::write(&tmp, text).map_err(|e| SaveError::Io(e.to_string()))?;
    std::fs::rename(&tmp, &path).map_err(|e| SaveError::Io(e.to_string()))?;
    let relative = path
        .strip_prefix(project_root)
        .unwrap_or(&path)
        .to_string_lossy()
        .replace('\\', "/");
    Ok(SaveOutcome {
        id: id.to_string(),
        path: relative,
        replaced,
    })
}

//! Registry of all known agent targets.
//!
//! Adding a new target = create `targets/<id>.rs` exporting an
//! `AgentTarget`, then add it to the array below. Order here is the
//! order they appear in the multiselect prompt, in `--target=all`,
//! and in `--print-config`'s help listing — keep it stable.

use super::antigravity::ANTIGRAVITY_TARGET;
use super::claude::CLAUDE_TARGET;
use super::codex::CODEX_TARGET;
use super::cursor::CURSOR_TARGET;
use super::gemini::GEMINI_TARGET;
use super::hermes::HERMES_TARGET;
use super::kiro::KIRO_TARGET;
use super::opencode::OPENCODE_TARGET;
use super::types::{AgentTarget, DetectionResult, Location, TargetId};
use crate::error::{CodeGraphError, Result};

pub static ALL_TARGETS: [&dyn AgentTarget; 8] = [
    &CLAUDE_TARGET,
    &CURSOR_TARGET,
    &CODEX_TARGET,
    &OPENCODE_TARGET,
    &HERMES_TARGET,
    &GEMINI_TARGET,
    &ANTIGRAVITY_TARGET,
    &KIRO_TARGET,
];

pub fn get_target(id: &str) -> Option<&'static dyn AgentTarget> {
    ALL_TARGETS.iter().find(|t| t.id().as_str() == id).copied()
}

pub fn list_target_ids() -> Vec<TargetId> {
    ALL_TARGETS.iter().map(|t| t.id()).collect()
}

/// A target zipped with its detection result.
pub struct TargetDetection {
    pub target: &'static dyn AgentTarget,
    pub detection: DetectionResult,
}

/// Run `detect()` for every target at the given location. Returns the
/// full registry zipped with detection results — orchestrator uses
/// this to seed the multiselect prompt with installed agents
/// pre-checked.
pub fn detect_all(loc: Location) -> Vec<TargetDetection> {
    ALL_TARGETS
        .iter()
        .map(|&target| TargetDetection {
            target,
            detection: target.detect(loc),
        })
        .collect()
}

/// Resolve a `--target=` flag value to a list of `AgentTarget`
/// instances. Accepts:
///
///   - `auto` — return all targets whose `detect().installed` is true,
///     or `['claude']` as a fallback if none detected (least-surprise
///     for existing users).
///   - `all` — every target in the registry.
///   - `none` — empty list (caller skips agent writes entirely).
///   - csv list — `'claude,cursor'` etc. Unknown ids error.
pub fn resolve_target_flag(value: &str, loc: Location) -> Result<Vec<&'static dyn AgentTarget>> {
    if value == "none" {
        return Ok(Vec::new());
    }
    if value == "all" {
        return Ok(ALL_TARGETS.to_vec());
    }
    if value == "auto" {
        let detected: Vec<&'static dyn AgentTarget> = detect_all(loc)
            .into_iter()
            .filter(|d| d.detection.installed)
            .map(|d| d.target)
            .collect();
        if !detected.is_empty() {
            return Ok(detected);
        }
        return Ok(get_target("claude").into_iter().collect());
    }

    let ids: Vec<&str> = value
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    let mut resolved: Vec<&'static dyn AgentTarget> = Vec::new();
    let mut unknown: Vec<&str> = Vec::new();
    for id in ids {
        match get_target(id) {
            Some(t) => resolved.push(t),
            None => unknown.push(id),
        }
    }
    if !unknown.is_empty() {
        let known = list_target_ids()
            .iter()
            .map(|t| t.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(CodeGraphError::config(format!(
            "Unknown --target id(s): {}. Known: {}, plus 'auto' / 'all' / 'none'.",
            unknown.join(", "),
            known,
        )));
    }
    Ok(resolved)
}

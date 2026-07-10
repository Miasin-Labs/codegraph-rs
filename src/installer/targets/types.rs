//! Agent target abstraction for the installer.
//!
//! Each MCP-capable agent (Claude Code, Cursor, Codex CLI, opencode, ...)
//! implements this trait so the installer orchestrator can write the
//! right MCP-server config + instructions file + permissions for that
//! agent without baking client-specific paths into core code. Adding a
//! new agent = one new file in `targets/` + one entry in `registry.rs`.
//!
//! Closes the Claude-locked installer issue (upstream #137). The
//! runtime MCP server is already agent-agnostic; this brings the
//! installer to the same surface.

use std::fmt;
use std::path::PathBuf;

use serde::Serialize;

/// Install location: user-wide (`global`) or project-local (`local`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Location {
    Global,
    Local,
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Location::Global => write!(f, "global"),
            Location::Local => write!(f, "local"),
        }
    }
}

impl std::str::FromStr for Location {
    type Err = crate::error::CodeGraphError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "global" => Ok(Location::Global),
            "local" => Ok(Location::Local),
            other => Err(crate::error::CodeGraphError::config(format!(
                "Invalid location: {other}. Use 'global' or 'local'."
            ))),
        }
    }
}

/// Stable string id used in the `--target` CLI flag and the registry
/// lookup. New targets add a value here when they're added to the
/// registry. Keep these short and lowercase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TargetId {
    Claude,
    Cursor,
    Codex,
    Opencode,
    Hermes,
    Gemini,
    Antigravity,
    Kiro,
}

impl TargetId {
    pub fn as_str(&self) -> &'static str {
        match self {
            TargetId::Claude => "claude",
            TargetId::Cursor => "cursor",
            TargetId::Codex => "codex",
            TargetId::Opencode => "opencode",
            TargetId::Hermes => "hermes",
            TargetId::Gemini => "gemini",
            TargetId::Antigravity => "antigravity",
            TargetId::Kiro => "kiro",
        }
    }
}

impl fmt::Display for TargetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Result of `target.detect(location)`.
///
/// `installed` is a best-effort heuristic that the agent's CLI / app /
/// config dir is present on this system — used to default the
/// multiselect prompt to "what's actually here." False positives are
/// acceptable (we still write); false negatives just mean the user
/// has to opt in manually.
///
/// `already_configured` reports whether codegraph has already been
/// wired into this target at this location — drives the
/// "Updated"-vs-"Added" log line and lets `--check` exit 0/1.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectionResult {
    pub installed: bool,
    pub already_configured: bool,
    /// Path inspected; surfaced in diagnostic / dry-run output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_path: Option<PathBuf>,
}

/// File action reported by `install` / `uninstall`.
///
/// `Unchanged` means we touched the file but its contents were already
/// what we'd write — used for byte-identical idempotent re-runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileAction {
    Created,
    Updated,
    Unchanged,
    Removed,
    NotFound,
    Kept,
}

impl FileAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            FileAction::Created => "created",
            FileAction::Updated => "updated",
            FileAction::Unchanged => "unchanged",
            FileAction::Removed => "removed",
            FileAction::NotFound => "not-found",
            FileAction::Kept => "kept",
        }
    }
}

/// One file touched (or inspected) by `install` / `uninstall`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileWrite {
    pub path: PathBuf,
    pub action: FileAction,
}

/// What `target.install(location)` actually changed on disk. The
/// orchestrator renders one log line per file using `action`.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteResult {
    pub files: Vec<FileWrite>,
    /// Optional one-line notes the orchestrator surfaces verbatim — e.g.
    /// "Restart Cursor to apply." Keep these short; multi-line goes in
    /// the README.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct InstallOptions {
    /// Whether to write the agent's permissions / auto-allow surface
    /// (Claude `settings.json`, others where applicable). When the
    /// target has no permissions concept this option is a no-op.
    pub auto_allow: bool,
    /// Front-load prompt hook (Claude `UserPromptSubmit`) that injects
    /// CodeGraph context for structural prompts. `Some(true)` installs it,
    /// `Some(false)` removes a prior install, and `None` leaves it untouched.
    /// Targets without a prompt-hook concept ignore this option.
    pub prompt_hook: Option<bool>,
}

/// Contract every agent target implements (TS `AgentTarget` interface).
pub trait AgentTarget: Send + Sync {
    /// Stable id; matches the `TargetId` enum.
    fn id(&self) -> TargetId;
    /// Human-readable name shown in prompts and log lines.
    fn display_name(&self) -> &'static str;
    /// Optional URL for "where do I learn more about this agent."
    fn docs_url(&self) -> Option<&'static str> {
        None
    }
    /// Whether this target supports the given install location.
    ///
    /// Some agents (Codex CLI as of 2026-05) have no project-local
    /// config concept — only a single `~/.codex/` dir. Returning false
    /// for an unsupported (target, location) pair lets the orchestrator
    /// skip cleanly with a clear message.
    fn supports_location(&self, loc: Location) -> bool;
    fn detect(&self, loc: Location) -> DetectionResult;
    fn install(&self, loc: Location, opts: &InstallOptions) -> WriteResult;
    /// Inverse of install. Removes only what install would have written;
    /// preserves sibling MCP servers, sibling permissions, and unrelated
    /// markdown sections. Must be safe to call when nothing was ever
    /// installed (returns `not-found` actions).
    fn uninstall(&self, loc: Location) -> WriteResult;
    /// Print the MCP-server snippet a user would paste manually for this
    /// target. Used by `codegraph install --print-config <id>` and by
    /// the README. Must NOT touch the filesystem.
    fn print_config(&self, loc: Location) -> String;
    /// Filesystem paths this target would write to at this location.
    fn describe_paths(&self, loc: Location) -> Vec<PathBuf>;
}

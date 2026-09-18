//! The two shapes of a tool call: raw (what an adapter recovered) and
//! stored (what a row holds). [`ToolEvent::from_raw`] is the only bridge,
//! and it redacts every string before deriving anything from it.

use std::fmt::Write as _;

use sha2::{Digest, Sha256};

use super::project::ProjectResolver;
use super::redact::redact;
use super::shell::CommandProfile;

/// One tool call as a source adapter recovered it.
///
/// Raw and possibly secret-bearing: it lives only in memory, between an
/// adapter and [`ToolEvent::from_raw`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawToolCall {
    /// The agent's own id for the call (`tool_use_id`, …). Only its hash is
    /// stored, as the row's dedupe key.
    pub native_id: String,
    pub ts: Option<String>,
    pub session: Option<String>,
    /// Canonical tool name (`Bash`, `Read`, `mcp__codegraph__codegraph_explore`).
    pub tool: String,
    /// Working directory the call ran in.
    pub cwd: Option<String>,
    /// Explicit project attribution; overrides everything derived from
    /// `cwd`, `cd`s and `file_path`.
    pub project: Option<String>,
    /// Shell command line (Bash-like tools).
    pub command: Option<String>,
    /// File the tool read or wrote (Read/Edit/Write).
    pub file_path: Option<String>,
    /// When the call ran, in epoch milliseconds, when the source has it as a
    /// number (else [`Self::ts`] is parsed).
    pub ts_ms: Option<i64>,
    /// More files the call edited (the files of an `apply_patch`).
    pub extra_paths: Vec<String>,
    /// Search pattern (Grep, Glob).
    pub pattern: Option<String>,
    /// Directory or file a search was scoped to.
    pub search_path: Option<String>,
    /// Symbol names a code-intelligence call asked about.
    pub symbols: Vec<String>,
    /// Free-text query of a code-intelligence call.
    pub query: Option<String>,
    /// First line a read started at (Read `offset`).
    pub line: Option<u32>,
    /// What the call returned, when the source records it.
    pub result: Option<CallResult>,
}

/// The result of a tool call, reduced to what the memory needs. The excerpt
/// lives only in memory: error codes and a signature hash are derived from
/// it, the text itself is never stored.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallResult {
    /// The tool reported an error (non-zero exit for a shell call).
    pub is_error: Option<bool>,
    /// Exit status of a shell call.
    pub exit_code: Option<i64>,
    /// Size of the output the agent received.
    pub output_bytes: u64,
    /// Bounded head of a build/test command's output.
    pub excerpt: Option<String>,
}

impl RawToolCall {
    /// When the call ran, in epoch milliseconds.
    pub fn when_ms(&self) -> Option<i64> {
        self.ts_ms
            .or_else(|| self.ts.as_deref().and_then(super::time::parse_rfc3339_ms))
    }
}

/// One stored, redaction-clean tool invocation — a `tool_events` row.
///
/// `#[non_exhaustive]`: outside this crate a row can only come from
/// [`ToolEvent::from_raw`], so nothing unredacted can be stored.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ToolEvent {
    /// Hash of `(source, native id)`: one row per real call, stable across re-ingests.
    pub call_key: String,
    /// Which agent's store the call came from (`jfc`, …).
    pub source: String,
    pub ts: Option<String>,
    pub session: Option<String>,
    /// Project root the call worked in (see [`ProjectResolver`]).
    pub project: Option<String>,
    pub tool_kind: String,
    /// The command a shell call was for (`cd /repo && cargo test` → `cargo`).
    pub primary_cmd: Option<String>,
    /// Normalized command chain (e.g. `cd | cargo | grep`).
    pub chain: Option<String>,
    /// File path the tool touched (Read/Edit/Write), when present.
    pub path: Option<String>,
    /// Whether a credential-shaped value was masked while building the row.
    pub redacted: bool,
}

impl ToolEvent {
    /// Build a row from a raw call: redact every raw string, derive the
    /// command profile and project from the *redacted* text, then pass each
    /// derived string through the redactor again before it is kept.
    pub fn from_raw(source: &str, raw: &RawToolCall, projects: &mut ProjectResolver) -> Self {
        let mut scrub = Scrubber::default();
        let command = scrub.opt(raw.command.as_deref());
        let file_path = scrub.opt(raw.file_path.as_deref());
        let cwd = scrub.opt(raw.cwd.as_deref());
        let explicit_project = scrub.opt(raw.project.as_deref());

        let profile = command
            .as_deref()
            .map(CommandProfile::parse)
            .unwrap_or_default();
        let project = explicit_project.or_else(|| {
            projects.resolve(cwd.as_deref(), profile.leading_cds(), file_path.as_deref())
        });

        Self {
            call_key: call_key(source, &raw.native_id),
            source: scrub.text(source),
            ts: scrub.opt(raw.ts.as_deref()),
            session: scrub.opt(raw.session.as_deref()),
            project: scrub.opt(project.as_deref()),
            tool_kind: scrub.text(&raw.tool),
            primary_cmd: scrub.opt(profile.primary()),
            chain: scrub.opt(profile.chain().as_deref()),
            path: file_path,
            redacted: scrub.hit,
        }
    }
}

/// Stable dedupe key for a call: a truncated SHA-256 of `source \0 native_id`
/// (32 hex chars). The native id itself is never stored.
pub fn call_key(source: &str, native_id: &str) -> String {
    let mut h = Sha256::new();
    h.update(source.as_bytes());
    h.update([0u8]);
    h.update(native_id.as_bytes());
    let digest = h.finalize();
    let mut key = String::with_capacity(32);
    for b in &digest[..16] {
        let _ = write!(key, "{b:02x}");
    }
    key
}

/// Runs strings through [`redact`], remembering whether anything was masked.
#[derive(Default)]
struct Scrubber {
    hit: bool,
}

impl Scrubber {
    fn text(&mut self, s: &str) -> String {
        let (out, hit) = redact(s);
        self.hit |= hit;
        out
    }

    fn opt(&mut self, s: Option<&str>) -> Option<String> {
        s.filter(|s| !s.is_empty()).map(|s| self.text(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bash(cmd: &str, cwd: &str) -> RawToolCall {
        RawToolCall {
            native_id: "toolu_1".into(),
            tool: "Bash".into(),
            cwd: Some(cwd.into()),
            command: Some(cmd.into()),
            ..Default::default()
        }
    }

    #[test]
    fn shell_rows_are_profiled_from_redacted_text() {
        let mut p = ProjectResolver::without_probe();
        let ev = ToolEvent::from_raw(
            "jfc",
            &bash(
                "cd /repo && echo 'hunter2!' | sudo -S cargo test",
                "/home/u",
            ),
            &mut p,
        );
        assert_eq!(ev.primary_cmd.as_deref(), Some("cargo"));
        assert_eq!(ev.chain.as_deref(), Some("cd | echo | cargo"));
        assert_eq!(
            ev.project.as_deref(),
            Some("/repo"),
            "leading cd decides the project"
        );
        assert!(ev.redacted);
        let dump = format!("{ev:?}");
        assert!(
            !dump.contains("hunter2"),
            "secret leaked into a row: {dump}"
        );
    }

    #[test]
    fn a_secret_command_word_never_reaches_the_chain() {
        let mut p = ProjectResolver::without_probe();
        let ev = ToolEvent::from_raw(
            "jfc",
            &bash(
                concat!("gh", "p_abcdefghijklmnopqrstuvwxyz0123 --help"),
                "/r",
            ),
            &mut p,
        );
        assert_eq!(ev.primary_cmd.as_deref(), Some("<REDACTED>"));
        assert!(ev.redacted);
    }

    #[test]
    fn file_rows_get_a_project_and_a_redacted_path() {
        let mut p = ProjectResolver::without_probe();
        let raw = RawToolCall {
            native_id: "toolu_2".into(),
            tool: "Read".into(),
            cwd: Some("/work/app".into()),
            file_path: Some("/work/app/users/jane.doe@example.com/notes.md".into()),
            ..Default::default()
        };
        let ev = ToolEvent::from_raw("jfc", &raw, &mut p);
        assert_eq!(ev.project.as_deref(), Some("/work/app"));
        assert_eq!(
            ev.path.as_deref(),
            Some("/work/app/users/<REDACTED>@example.com/notes.md")
        );
        assert_eq!(ev.primary_cmd, None);
        assert!(ev.redacted);
    }

    #[test]
    fn explicit_project_overrides_derivation() {
        let mut p = ProjectResolver::without_probe();
        let raw = RawToolCall {
            project: Some("/pinned".into()),
            ..bash("cd /elsewhere && ls", "/cwd")
        };
        assert_eq!(
            ToolEvent::from_raw("jfc", &raw, &mut p).project.as_deref(),
            Some("/pinned")
        );
    }

    #[test]
    fn call_key_is_stable_and_source_scoped() {
        assert_eq!(call_key("jfc", "toolu_1"), call_key("jfc", "toolu_1"));
        assert_ne!(
            call_key("jfc", "toolu_1"),
            call_key("claude-code", "toolu_1")
        );
        assert_ne!(call_key("jfc", "toolu_1"), call_key("jfc", "toolu_2"));
        assert_eq!(call_key("jfc", "x").len(), 32);
    }
}

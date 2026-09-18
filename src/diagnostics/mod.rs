//! Compiler diagnostics from the project's own toolchain.
//!
//! The graph cannot tell broken code from code its resolver did not
//! understand, so "what's broken?" is answered by the compiler: `cargo
//! check`/`cargo clippy --message-format=json` for Rust and `tsc --noEmit` for
//! TypeScript. Agents in the mined sessions called `lsp_diagnostics` 4,760
//! times for exactly this.
//!
//! A check can take minutes on a large workspace, and MCP calls are
//! time-boxed, so a run is started detached with its output going to
//! `.codegraph/diagnostics/`, polled up to a deadline, and — if still going —
//! picked up by the next call instead of being restarted.

mod cargo;
mod run;
mod tsc;

use std::path::Path;

pub use run::{DiagnosticsRun, RunStatus, check_or_poll};
use serde::Serialize;

/// Which checker to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Checker {
    /// `cargo check` — compiler errors and warnings.
    Check,
    /// `cargo clippy` — compiler diagnostics plus clippy lints.
    Clippy,
    /// `tsc --noEmit` with the project's own TypeScript.
    Tsc,
}

impl Checker {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Clippy => "clippy",
            Self::Tsc => "tsc",
        }
    }

    /// The checker to use for `root` when the caller did not pick one, or
    /// validate an explicit pick against what the project has.
    pub fn for_project(root: &Path, requested: Option<&str>) -> Result<Self, String> {
        let cargo = root.join("Cargo.toml").is_file();
        let tsc = tsc::binary(root).is_some();
        match requested {
            Some("check") | Some("clippy") if !cargo => Err(format!(
                "no Cargo.toml at {}; `{}` needs a Cargo project",
                root.display(),
                requested.unwrap_or_default()
            )),
            Some("check") => Ok(Self::Check),
            Some("clippy") => Ok(Self::Clippy),
            Some("tsc") if !tsc => Err(format!(
                "no node_modules/.bin/tsc under {}; install the project's TypeScript first",
                root.display()
            )),
            Some("tsc") => Ok(Self::Tsc),
            Some(other) => Err(format!(
                "unknown checker `{other}` (expected check, clippy, or tsc)"
            )),
            None if cargo => Ok(Self::Check),
            None if tsc => Ok(Self::Tsc),
            None => Err(format!(
                "no supported checker for {}: needs a Cargo.toml (cargo check/clippy) or \
                 node_modules/.bin/tsc (TypeScript)",
                root.display()
            )),
        }
    }
}

/// How serious a diagnostic is, as the compiler reported it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Note,
}

/// One compiler diagnostic at its primary location, project-relative.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub severity: Severity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub message: String,
    pub file: String,
    pub line: u32,
    pub column: u32,
}

/// Parse a finished run's captured output with the checker's parser.
fn parse(checker: Checker, root: &Path, stdout: &str) -> Vec<Diagnostic> {
    match checker {
        Checker::Check | Checker::Clippy => cargo::parse(root, stdout),
        Checker::Tsc => tsc::parse(root, stdout),
    }
}

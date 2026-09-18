//! `cargo check|clippy --message-format=json` output.

use std::path::Path;
use std::process::Command;

use serde_json::Value;

use super::{Checker, Diagnostic, Severity};

pub(super) fn command(checker: Checker) -> Command {
    let mut command = Command::new("cargo");
    command
        .arg(match checker {
            Checker::Clippy => "clippy",
            _ => "check",
        })
        // Offline: a diagnostics call must not fetch code. A project whose
        // dependencies are not in the local cache reports that failure.
        .args([
            "--message-format=json",
            "--workspace",
            "--all-targets",
            "--offline",
        ])
        .env("CARGO_TERM_COLOR", "never");
    command
}

/// Compiler messages at their primary span, for files inside `root`.
/// Diagnostics located only in dependencies or the standard library are
/// dropped: they are not something the project can fix in place.
pub(super) fn parse(root: &Path, stdout: &str) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if record["reason"] != "compiler-message" {
            continue;
        }
        let message = &record["message"];
        let severity = match message["level"].as_str() {
            Some("error") | Some("error: internal compiler error") => Severity::Error,
            Some("warning") => Severity::Warning,
            Some("note") | Some("help") => Severity::Note,
            _ => continue,
        };
        let Some(span) = message["spans"]
            .as_array()
            .and_then(|spans| spans.iter().find(|span| span["is_primary"] == true))
        else {
            continue;
        };
        let Some(file) = span["file_name"].as_str().and_then(|f| relative(root, f)) else {
            continue;
        };
        out.push(Diagnostic {
            severity,
            code: message["code"]["code"].as_str().map(str::to_string),
            message: message["message"].as_str().unwrap_or_default().to_string(),
            file,
            line: span["line_start"].as_u64().unwrap_or(0) as u32,
            column: span["column_start"].as_u64().unwrap_or(0) as u32,
        });
    }
    out.sort_by(|a, b| (a.severity, &a.file, a.line).cmp(&(b.severity, &b.file, b.line)));
    out.dedup();
    out
}

/// Cargo reports workspace files relative to the workspace root and
/// everything else (registry crates, std) absolute.
fn relative(root: &Path, file: &str) -> Option<String> {
    let path = Path::new(file);
    if path.is_relative() {
        return Some(file.replace('\\', "/"));
    }
    path.strip_prefix(root)
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_primary_spans_inside_the_project() {
        let out = [
            r#"{"reason":"compiler-artifact","target":{}}"#,
            r#"{"reason":"compiler-message","message":{"level":"error","code":{"code":"E0308"},"message":"mismatched types","spans":[{"file_name":"src/lib.rs","line_start":3,"column_start":9,"is_primary":true},{"file_name":"src/lib.rs","line_start":1,"column_start":1,"is_primary":false}]}}"#,
            r#"{"reason":"compiler-message","message":{"level":"warning","code":{"code":"unused_variables"},"message":"unused variable: `x`","spans":[{"file_name":"/proj/src/a.rs","line_start":7,"column_start":5,"is_primary":true}]}}"#,
            r#"{"reason":"compiler-message","message":{"level":"warning","code":null,"message":"in a dependency","spans":[{"file_name":"/home/u/.cargo/registry/src/x/lib.rs","line_start":1,"column_start":1,"is_primary":true}]}}"#,
            r#"{"reason":"compiler-message","message":{"level":"warning","code":null,"message":"3 warnings emitted","spans":[]}}"#,
            "not json",
        ]
        .join("\n");
        let diagnostics = parse(Path::new("/proj"), &out);
        assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
        assert_eq!(diagnostics[0].severity, Severity::Error);
        assert_eq!(diagnostics[0].code.as_deref(), Some("E0308"));
        assert_eq!(
            (diagnostics[0].file.as_str(), diagnostics[0].line),
            ("src/lib.rs", 3)
        );
        assert_eq!(diagnostics[1].file, "src/a.rs");
    }
}

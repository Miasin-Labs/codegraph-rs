//! `tsc --noEmit --pretty false` output.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use regex::Regex;

use super::{Diagnostic, Severity};

/// The project's own TypeScript compiler. Never a global or `npx` download:
/// the version must match the project, and a check must not fetch code.
pub(super) fn binary(root: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) { "tsc.cmd" } else { "tsc" };
    let path = root.join("node_modules").join(".bin").join(name);
    path.is_file().then_some(path)
}

pub(super) fn command(root: &Path) -> Option<Command> {
    let mut command = Command::new(binary(root)?);
    command.args(["--noEmit", "--pretty", "false"]);
    Some(command)
}

/// `src/a.ts(12,5): error TS2322: Type 'string' is not assignable…`
static LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<file>.+?)\((?P<line>\d+),(?P<col>\d+)\): (?P<sev>error|warning) (?P<code>TS\d+): (?P<msg>.*)$")
        .expect("valid tsc line regex")
});

pub(super) fn parse(root: &Path, stdout: &str) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let Some(caps) = LINE_RE.captures(line.trim_end()) else {
            continue;
        };
        let file = caps["file"].replace('\\', "/");
        let file = Path::new(&file)
            .strip_prefix(root)
            .map_or(file.clone(), |p| p.to_string_lossy().into_owned());
        if file.contains("node_modules/") {
            continue;
        }
        out.push(Diagnostic {
            severity: if &caps["sev"] == "error" {
                Severity::Error
            } else {
                Severity::Warning
            },
            code: Some(caps["code"].to_string()),
            message: caps["msg"].to_string(),
            file,
            line: caps["line"].parse().unwrap_or(0),
            column: caps["col"].parse().unwrap_or(0),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tsc_lines_and_skips_dependencies() {
        let out = "src/a.ts(12,5): error TS2322: Type 'string' is not assignable to type 'number'.\n\
                   node_modules/x/index.d.ts(1,1): error TS1005: ';' expected.\n\
                   Found 2 errors.\n";
        let diagnostics = parse(Path::new("/proj"), out);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code.as_deref(), Some("TS2322"));
        assert_eq!((diagnostics[0].line, diagnostics[0].column), (12, 5));
        assert_eq!(diagnostics[0].file, "src/a.ts");
    }
}

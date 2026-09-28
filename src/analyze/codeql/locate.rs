//! Finding the CodeQL CLI: `CODEGRAPH_CODEQL` (the `codeql` launcher or a
//! bundle directory), else `codeql` on PATH, else the bundle unpacked in
//! the tools cache (`~/.cache/codegraph-tools/codeql/codeql/codeql`;
//! `CODEGRAPH_TOOLS_DIR` moves `~/.cache/codegraph-tools`). Nothing is ever
//! downloaded or installed: a missing CLI is reported with how to get it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The CLI found, and what it says about itself.
#[derive(Debug, Clone)]
pub struct CodeqlTool {
    /// The `codeql` launcher.
    pub exe: PathBuf,
    /// The distribution directory (holds `java/`, `qlpacks/`…).
    pub dist: PathBuf,
    /// `codeql version --format=terse` (`2.27.1`).
    pub version: String,
}

/// What `analyze codeql` says when the CLI is not found.
pub const INSTALL_HINT: &str = "CodeQL CLI not found. Set CODEGRAPH_CODEQL to the `codeql` \
    launcher (or its bundle directory), put `codeql` on PATH, or unpack a CodeQL bundle \
    (codeql-bundle-<platform>.tar.gz from https://github.com/github/codeql-action/releases) \
    into ~/.cache/codegraph-tools/codeql/. Its license permits use on open-source code, \
    academic research, and testing or demonstrating the software — not other private code \
    (https://securitylab.github.com/tools/codeql/license).";

fn launcher_name() -> &'static str {
    if cfg!(windows) {
        "codeql.exe"
    } else {
        "codeql"
    }
}

/// The launcher inside `dir` when `dir` is a distribution (`dir/codeql`)
/// or a directory holding one (`dir/codeql/codeql`).
fn in_dir(dir: &Path) -> Option<PathBuf> {
    [
        dir.join(launcher_name()),
        dir.join("codeql").join(launcher_name()),
    ]
    .into_iter()
    .find(|candidate| candidate.is_file())
}

/// The tools cache the bundle may be unpacked in.
pub fn tools_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CODEGRAPH_TOOLS_DIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    dirs::cache_dir().map(|cache| cache.join("codegraph-tools"))
}

/// Candidate launchers, in the order they are tried.
fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(value) = std::env::var_os("CODEGRAPH_CODEQL").filter(|v| !v.is_empty()) {
        let path = PathBuf::from(value);
        if path.is_dir() {
            out.extend(in_dir(&path));
        } else {
            out.push(path);
        }
        // An explicit choice is the only one tried.
        return out;
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(launcher_name());
            if candidate.is_file() {
                out.push(candidate);
                break;
            }
        }
    }
    if let Some(tools) = tools_dir() {
        out.extend(in_dir(&tools.join("codeql")));
    }
    out
}

/// Find the CLI and ask its version; `Err` says why nothing usable was
/// found (with [`INSTALL_HINT`]).
pub fn find() -> Result<CodeqlTool, String> {
    let mut problems = Vec::new();
    for exe in candidates() {
        match version(&exe) {
            Ok(version) => {
                let dist = exe
                    .canonicalize()
                    .unwrap_or_else(|_| exe.clone())
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_default();
                return Ok(CodeqlTool { exe, dist, version });
            }
            Err(why) => problems.push(format!("{}: {why}", exe.display())),
        }
    }
    if problems.is_empty() {
        Err(INSTALL_HINT.to_string())
    } else {
        Err(format!("{} ({INSTALL_HINT})", problems.join("; ")))
    }
}

fn version(exe: &Path) -> Result<String, String> {
    let out = Command::new(exe)
        .args(["version", "--format=terse"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`codeql version` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "`codeql version` printed nothing".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bundle_directory_holds_the_launcher() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(in_dir(dir.path()), None);
        let dist = dir.path().join("codeql");
        std::fs::create_dir_all(&dist).unwrap();
        std::fs::write(dist.join(launcher_name()), "").unwrap();
        assert_eq!(in_dir(dir.path()), Some(dist.join(launcher_name())));
        assert_eq!(in_dir(&dist), Some(dist.join(launcher_name())));
    }

    #[test]
    fn a_launcher_that_does_not_run_is_not_a_tool() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("codeql");
        std::fs::write(&fake, "not a program").unwrap();
        assert!(version(&fake).is_err());
    }
}

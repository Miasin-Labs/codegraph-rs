//! Go module path detection.
//!
//! A Go monorepo's cross-package calls (`pkga.FuncX(...)`) only resolve when
//! the resolver knows the project's module path (the `module ...` directive
//! in `go.mod`). Without it, `isExternalImport` treats every in-module import
//! — `github.com/example/myproject/pkga` — as a third-party package, so
//! resolution falls through to name-matching with path proximity and returns
//! a tiny fraction of the real call sites. See issue #388.
//!
//! Ported from `src/resolution/go-module.ts`. The `GoModule` data type is
//! defined in [`super::types`] (see `notes/resolution-types.md`); this file
//! re-exports it and implements only the loader.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

pub use super::types::GoModule;

/// `module <path>` is the first non-comment directive in any valid go.mod.
static MODULE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*module\s+(\S+)\s*$").expect("valid regex"));

/// Read the `go.mod` file at the project root and extract the module path.
/// Returns `None` if no `go.mod` exists or it has no `module` directive.
///
/// Limitation: only the project-root `go.mod` is read. Nested `go.mod` files
/// (Go workspaces, monorepos with multiple modules) are not yet resolved —
/// a follow-up if a real repro shows up.
pub fn load_go_module(project_root: &str) -> Option<GoModule> {
    let go_mod_path = Path::new(project_root).join("go.mod");
    let content = std::fs::read_to_string(&go_mod_path).ok()?;
    // Strip line comments so a `// module foo` doesn't false-match.
    let stripped = strip_line_comments(&content);
    let m = MODULE_RE.captures(&stripped)?;
    // Strip optional quoting around the module path.
    let raw = m.get(1).expect("group 1").as_str();
    let module_path = strip_outer_quotes(raw);
    if module_path.is_empty() {
        return None;
    }
    Some(GoModule {
        module_path: module_path.to_string(),
        root_dir: PathBuf::from(project_root),
    })
}

/// TS: `content.replace(/\/\/[^\n]*/g, '')`.
fn strip_line_comments(content: &str) -> String {
    static LINE_COMMENT_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"//[^\n]*").expect("valid regex"));
    LINE_COMMENT_RE.replace_all(content, "").into_owned()
}

/// TS: `.replace(/^["']|["']$/g, '')` — removes at most one leading and one
/// trailing quote character (either kind, independently).
fn strip_outer_quotes(s: &str) -> &str {
    let s = s.strip_prefix(['"', '\'']).unwrap_or(s);
    s.strip_suffix(['"', '\'']).unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn loads_module_path_from_go_mod() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("go.mod"),
            "module github.com/example/myproject\n\ngo 1.21\n",
        )
        .unwrap();
        let m = load_go_module(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(m.module_path, "github.com/example/myproject");
        assert_eq!(m.root_dir, dir.path().to_path_buf());
    }

    #[test]
    fn returns_none_without_go_mod() {
        let dir = tempdir().unwrap();
        assert!(load_go_module(dir.path().to_str().unwrap()).is_none());
    }

    #[test]
    fn returns_none_without_module_directive() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("go.mod"), "go 1.21\n").unwrap();
        assert!(load_go_module(dir.path().to_str().unwrap()).is_none());
    }

    #[test]
    fn ignores_commented_out_module_directive() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("go.mod"),
            "// module commented/out\nmodule real/module\n",
        )
        .unwrap();
        let m = load_go_module(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(m.module_path, "real/module");
    }

    #[test]
    fn comment_only_module_line_yields_none() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("go.mod"), "// module foo\ngo 1.21\n").unwrap();
        assert!(load_go_module(dir.path().to_str().unwrap()).is_none());
    }

    #[test]
    fn strips_quotes_around_module_path() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("go.mod"), "module \"example.com/quoted\"\n").unwrap();
        let m = load_go_module(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(m.module_path, "example.com/quoted");
    }

    #[test]
    fn trailing_inline_comment_on_module_line() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("go.mod"),
            "module example.com/foo // the module\n",
        )
        .unwrap();
        let m = load_go_module(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(m.module_path, "example.com/foo");
    }
}

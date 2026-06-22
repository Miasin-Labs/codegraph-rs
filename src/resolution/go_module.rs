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

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use walkdir::{DirEntry, WalkDir};

pub use super::types::{GoModule, GoModuleRoot};
use crate::utils::normalize_path;

/// `module <path>` is the first non-comment directive in any valid go.mod.
static MODULE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*module\s+(\S+)\s*$").expect("valid regex"));

/// Discover `go.mod` files under the project root and extract module paths.
pub fn load_go_module(project_root: &str) -> Option<GoModule> {
    let project_root = Path::new(project_root);
    let mut module_roots = discover_go_module_roots(project_root);
    if module_roots.is_empty() {
        return None;
    }

    let primary = select_primary_module_root(project_root, &module_roots).clone();
    sort_module_roots_for_matching(&mut module_roots);

    Some(GoModule {
        module_path: primary.module_path,
        root_dir: primary.root_dir,
        module_roots,
    })
}

pub fn go_package_dir_for_import(
    module: &GoModule,
    import_path: &str,
    project_root: &str,
) -> Option<String> {
    let module_root = module.matching_root(import_path)?;
    let suffix = module_root.import_suffix(import_path)?;
    let root_rel = module_root_relative_to_project(&module_root.root_dir, Path::new(project_root));
    match (root_rel.is_empty(), suffix.is_empty()) {
        (true, true) => Some(String::new()),
        (true, false) => Some(suffix.to_string()),
        (false, true) => Some(root_rel),
        (false, false) => Some(format!("{root_rel}/{suffix}")),
    }
}

fn discover_go_module_roots(project_root: &Path) -> Vec<GoModuleRoot> {
    WalkDir::new(project_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(should_descend)
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file() && entry.file_name() == "go.mod")
        .filter_map(|entry| load_go_module_root(entry.path()))
        .collect()
}

fn should_descend(entry: &DirEntry) -> bool {
    if entry.depth() == 0 || !entry.file_type().is_dir() {
        return true;
    }
    !matches!(
        entry.file_name().to_string_lossy().as_ref(),
        ".git" | ".codegraph" | "target" | "node_modules" | "vendor"
    )
}

fn load_go_module_root(go_mod_path: &Path) -> Option<GoModuleRoot> {
    let content = std::fs::read_to_string(go_mod_path).ok()?;
    let module_path = parse_module_path(&content)?;
    Some(GoModuleRoot {
        module_path,
        root_dir: go_mod_path.parent()?.to_path_buf(),
    })
}

fn parse_module_path(content: &str) -> Option<String> {
    let stripped = strip_line_comments(content);
    let m = MODULE_RE.captures(&stripped)?;
    let raw = m.get(1).expect("group 1").as_str();
    let module_path = strip_outer_quotes(raw);
    if module_path.is_empty() {
        return None;
    }
    Some(module_path.to_string())
}

fn select_primary_module_root<'a>(
    project_root: &Path,
    module_roots: &'a [GoModuleRoot],
) -> &'a GoModuleRoot {
    module_roots
        .iter()
        .min_by(|a, b| {
            let a_rel = module_root_relative_to_project(&a.root_dir, project_root);
            let b_rel = module_root_relative_to_project(&b.root_dir, project_root);
            path_depth(&a_rel)
                .cmp(&path_depth(&b_rel))
                .then_with(|| a_rel.cmp(&b_rel))
                .then_with(|| a.module_path.cmp(&b.module_path))
        })
        .expect("non-empty module roots")
}

fn sort_module_roots_for_matching(module_roots: &mut [GoModuleRoot]) {
    module_roots.sort_by(|a, b| {
        b.module_path
            .len()
            .cmp(&a.module_path.len())
            .then_with(|| a.module_path.cmp(&b.module_path))
            .then_with(|| a.root_dir.cmp(&b.root_dir))
    });
}

fn module_root_relative_to_project(root_dir: &Path, project_root: &Path) -> String {
    if let Ok(rel) = root_dir.strip_prefix(project_root) {
        return clean_relative_path(rel);
    }

    let root = normalize_path(root_dir.to_string_lossy().as_ref());
    let project = normalize_path(project_root.to_string_lossy().as_ref());
    let project = project.trim_end_matches('/');
    if root == project {
        return String::new();
    }
    root.strip_prefix(&format!("{project}/"))
        .map(str::to_string)
        .unwrap_or_default()
}

fn clean_relative_path(path: &Path) -> String {
    let normalized = normalize_path(path.to_string_lossy().as_ref());
    if normalized == "." {
        String::new()
    } else {
        normalized
    }
}

fn path_depth(path: &str) -> usize {
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .count()
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

    #[test]
    fn loads_first_nested_module_when_project_root_has_no_go_mod() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("go")).unwrap();
        fs::create_dir_all(dir.path().join("proto")).unwrap();
        fs::write(
            dir.path().join("go/go.mod"),
            "module github.com/dolthub/dolt/go\n\ngo 1.24\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("proto/go.mod"),
            "module github.com/dolthub/dolt/proto\n\ngo 1.24\n",
        )
        .unwrap();

        let m = load_go_module(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(m.module_path, "github.com/dolthub/dolt/go");
        assert_eq!(m.root_dir, dir.path().join("go"));
        assert_eq!(m.module_roots.len(), 2);
        assert!(m.module_roots.iter().any(|module_root| {
            module_root.module_path == "github.com/dolthub/dolt/proto"
                && module_root.root_dir == dir.path().join("proto")
        }));
    }
}

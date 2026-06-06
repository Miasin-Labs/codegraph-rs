//! JS/TS workspace (monorepo) package resolution.
//!
//! npm / yarn / bun read member packages from the root `package.json`
//! `workspaces` field; pnpm from `pnpm-workspace.yaml`. A cross-package
//! import like `@scope/ui/widgets` is LOCAL to the monorepo, but to a
//! single-package resolver it looks exactly like a third-party npm
//! specifier — so `isExternalImport` flags it external and the
//! consumer↔definition edge is never created. For component barrels
//! (`export { default as X } from './x.svelte'`) that surfaces as a false
//! `0 callers` on a live component (issue #629).
//!
//! This module maps each member package's declared `name` to its
//! directory so the resolver can rewrite `@scope/ui/widgets` →
//! `packages/ui/widgets` and then run normal extension/index resolution.
//!
//! Scope deliberately small for v1 (mirrors path-aliases.ts):
//!   - reads `workspaces` (array OR `{ packages: [...] }`) from package.json,
//!     plus a minimal `pnpm-workspace.yaml` `packages:` list
//!   - expands one level of `*` / `**` globs (`packages/*`, `apps/*`)
//!   - subpath resolution is directory-based (`@scope/ui/sub` → `<ui>/sub`);
//!     it does NOT yet honour a member's `exports` map or `main` field
//!   - returns `None` when the project declares no workspaces, so single-
//!     package repos pay nothing and see no behaviour change.
//!
//! Ported from `src/resolution/workspace-packages.ts`. The
//! `WorkspacePackages` data type is defined in [`super::types`] (see
//! `notes/resolution-types.md`); this file re-exports it and implements
//! only the loader + import rewrite.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

pub use super::types::WorkspacePackages;
use crate::error::log_debug;

/// Load workspace member packages for `project_root`. Returns `None` when
/// the project declares no workspaces (the common single-package case) —
/// callers then skip all workspace logic.
///
/// Cheap to call repeatedly only via the resolver's per-instance cache;
/// this function itself touches the filesystem, so the resolver memoises it
/// the same way it does `load_project_aliases` / `load_go_module`.
pub fn load_workspace_packages(project_root: &str) -> Option<WorkspacePackages> {
    let patterns = read_workspace_globs(project_root);
    if patterns.is_empty() {
        return None;
    }

    let mut by_name: HashMap<String, String> = HashMap::new();
    for pattern in &patterns {
        for dir in expand_workspace_glob(project_root, pattern) {
            let pkg_name = read_package_name(&Path::new(project_root).join(&dir));
            // First declaration wins — workspace patterns are tried in order.
            if let Some(name) = pkg_name {
                by_name.entry(name).or_insert(dir);
            }
        }
    }
    if by_name.is_empty() {
        return None;
    }

    log_debug(
        "workspace packages loaded",
        Some(&serde_json::json!({ "count": by_name.len() })),
    );
    Some(WorkspacePackages { by_name })
}

/// Rewrite a bare workspace import to a path relative to projectRoot,
/// WITHOUT an extension — the caller applies the language's extension/index
/// resolution. `@scope/ui/widgets` → `packages/ui/widgets`; the bare package
/// name `@scope/ui` → its directory. Returns `None` when no member package
/// name matches.
pub fn resolve_workspace_import(import_path: &str, ws: &WorkspacePackages) -> Option<String> {
    // Longest matching package name wins, so `@scope/ui/core` prefers a
    // `@scope/ui/core` package over a `@scope/ui` one when both exist.
    let mut best_name: Option<&str> = None;
    for name in ws.by_name.keys() {
        if (import_path == name || import_path.starts_with(&format!("{name}/")))
            && best_name.is_none_or(|b| name.len() > b.len())
        {
            best_name = Some(name);
        }
    }
    let best_name = best_name?;
    let dir = ws.by_name.get(best_name).expect("key just found");
    let subpath = &import_path[best_name.len()..]; // '' or '/widgets'
    Some(collapse_slashes(&format!("{dir}{subpath}")))
}

/// TS: `.replace(/\/{2,}/g, '/')`.
fn collapse_slashes(s: &str) -> String {
    static MULTI_SLASH_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"/{2,}").expect("valid regex"));
    MULTI_SLASH_RE.replace_all(s, "/").into_owned()
}

/// Read workspace glob patterns from package.json + pnpm-workspace.yaml.
fn read_workspace_globs(project_root: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();

    // package.json `workspaces` (npm / yarn / bun): array, or Yarn's
    // `{ packages: [...], nohoist: [...] }` object form.
    if let Ok(raw) = fs::read_to_string(Path::new(project_root).join("package.json")) {
        if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&raw) {
            let ws = pkg.get("workspaces");
            if let Some(arr) = ws.and_then(|w| w.as_array()) {
                out.extend(arr.iter().filter_map(|w| w.as_str().map(String::from)));
            } else if let Some(arr) = ws
                .and_then(|w| w.get("packages"))
                .and_then(|p| p.as_array())
            {
                out.extend(arr.iter().filter_map(|w| w.as_str().map(String::from)));
            }
        }
        // else: invalid package.json — not a workspace root
    }

    // pnpm-workspace.yaml `packages:` list. Parsed with a minimal line
    // scanner so we don't pull in a YAML dependency.
    if let Ok(yaml) = fs::read_to_string(Path::new(project_root).join("pnpm-workspace.yaml")) {
        out.extend(parse_pnpm_packages(&yaml));
    }

    out
}

/// Minimal pnpm-workspace.yaml `packages:` extractor. Handles the only shape
/// pnpm actually uses:
///   packages:
///     - 'packages/*'
///     - "apps/*"
///     - tools/build
fn parse_pnpm_packages(yaml: &str) -> Vec<String> {
    static PACKAGES_KEY_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s*packages\s*:").expect("valid regex"));
    static ITEM_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s*-\s*(.+?)\s*$").expect("valid regex"));

    let mut out: Vec<String> = Vec::new();
    // TS: yaml.split(/\r?\n/)
    let mut in_packages = false;
    for line in yaml.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if PACKAGES_KEY_RE.is_match(line) {
            in_packages = true;
            continue;
        }
        if in_packages {
            if let Some(item) = ITEM_RE.captures(line) {
                let raw = item.get(1).expect("group 1").as_str();
                out.push(strip_outer_quotes(raw).to_string());
                continue;
            }
            // A non-list, non-blank line ends the `packages:` block.
            if !line.trim().is_empty() && !line.starts_with(|c: char| c.is_whitespace()) {
                in_packages = false;
            }
        }
    }
    out
}

/// TS: `.replace(/^['"]|['"]$/g, '')` — removes at most one leading and one
/// trailing quote character (either kind, independently).
fn strip_outer_quotes(s: &str) -> &str {
    let s = s.strip_prefix(['\'', '"']).unwrap_or(s);
    s.strip_suffix(['\'', '"']).unwrap_or(s)
}

/// Expand one level of a `packages/*` / `apps/**` glob to member dirs.
fn expand_workspace_glob(project_root: &str, pattern: &str) -> Vec<String> {
    let norm = pattern.replace('\\', "/");
    let norm = norm.trim_end_matches('/');
    let star = match norm.find('*') {
        Some(i) => i,
        None => return vec![norm.to_string()], // exact directory
    };

    // Everything before the wildcard segment is the base to enumerate.
    let base = norm[..star].trim_end_matches('/');
    let read_dir = match fs::read_dir(Path::new(project_root).join(base)) {
        Ok(rd) => rd,
        Err(_) => return Vec::new(),
    };
    let mut out: Vec<String> = Vec::new();
    for entry in read_dir.flatten() {
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_dir || name.starts_with('.') || name == "node_modules" {
            continue;
        }
        out.push(if base.is_empty() {
            name
        } else {
            format!("{base}/{name}")
        });
    }
    out
}

/// Read the `name` field from a member directory's package.json.
fn read_package_name(dir_abs: &Path) -> Option<String> {
    let raw = fs::read_to_string(dir_abs.join("package.json")).ok()?;
    let pkg = serde_json::from_str::<serde_json::Value>(&raw).ok()?;
    match pkg.get("name").and_then(|n| n.as_str()) {
        Some(name) if !name.is_empty() => Some(name.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    fn write_pkg(dir: &Path, name: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join("package.json"),
            format!("{{\"name\": \"{name}\"}}"),
        )
        .unwrap();
    }

    #[test]
    fn loads_npm_workspaces_array() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::write(
            root.join("package.json"),
            r#"{"name": "root", "workspaces": ["packages/*"]}"#,
        )
        .unwrap();
        write_pkg(&root.join("packages/ui"), "@scope/ui");
        write_pkg(&root.join("packages/core"), "@scope/core");

        let ws = load_workspace_packages(root.to_str().unwrap()).unwrap();
        assert_eq!(ws.by_name.get("@scope/ui").unwrap(), "packages/ui");
        assert_eq!(ws.by_name.get("@scope/core").unwrap(), "packages/core");
    }

    #[test]
    fn loads_yarn_object_form() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::write(
            root.join("package.json"),
            r#"{"workspaces": {"packages": ["apps/*"], "nohoist": ["**/x"]}}"#,
        )
        .unwrap();
        write_pkg(&root.join("apps/web"), "web");

        let ws = load_workspace_packages(root.to_str().unwrap()).unwrap();
        assert_eq!(ws.by_name.get("web").unwrap(), "apps/web");
    }

    #[test]
    fn loads_pnpm_workspace_yaml() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::write(
            root.join("pnpm-workspace.yaml"),
            "packages:\n  - 'packages/*'\n  - \"apps/*\"\n  - tools/build\n",
        )
        .unwrap();
        write_pkg(&root.join("packages/ui"), "@scope/ui");
        write_pkg(&root.join("apps/site"), "site");
        write_pkg(&root.join("tools/build"), "@scope/build");

        let ws = load_workspace_packages(root.to_str().unwrap()).unwrap();
        assert_eq!(ws.by_name.get("@scope/ui").unwrap(), "packages/ui");
        assert_eq!(ws.by_name.get("site").unwrap(), "apps/site");
        assert_eq!(ws.by_name.get("@scope/build").unwrap(), "tools/build");
    }

    #[test]
    fn returns_none_for_single_package_repo() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("package.json"), r#"{"name": "solo"}"#).unwrap();
        assert!(load_workspace_packages(root.to_str().unwrap()).is_none());
    }

    #[test]
    fn returns_none_when_globs_match_no_named_packages() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::write(
            root.join("package.json"),
            r#"{"workspaces": ["packages/*"]}"#,
        )
        .unwrap();
        fs::create_dir_all(root.join("packages/empty")).unwrap(); // no package.json
        assert!(load_workspace_packages(root.to_str().unwrap()).is_none());
    }

    #[test]
    fn first_declaration_wins_on_duplicate_names() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::write(
            root.join("package.json"),
            r#"{"workspaces": ["packages/*", "other/*"]}"#,
        )
        .unwrap();
        write_pkg(&root.join("packages/a"), "dup");
        write_pkg(&root.join("other/b"), "dup");

        let ws = load_workspace_packages(root.to_str().unwrap()).unwrap();
        assert_eq!(ws.by_name.get("dup").unwrap(), "packages/a");
    }

    #[test]
    fn skips_dot_dirs_and_node_modules() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("package.json"), r#"{"workspaces": ["*"]}"#).unwrap();
        write_pkg(&root.join(".hidden"), "hidden");
        write_pkg(&root.join("node_modules"), "nm");
        write_pkg(&root.join("real"), "real");

        let ws = load_workspace_packages(root.to_str().unwrap()).unwrap();
        assert_eq!(ws.by_name.len(), 1);
        assert_eq!(ws.by_name.get("real").unwrap(), "real");
    }

    #[test]
    fn resolve_workspace_import_rewrites_subpaths() {
        let mut by_name = HashMap::new();
        by_name.insert("@scope/ui".to_string(), "packages/ui".to_string());
        let ws = WorkspacePackages { by_name };

        assert_eq!(
            resolve_workspace_import("@scope/ui", &ws).as_deref(),
            Some("packages/ui")
        );
        assert_eq!(
            resolve_workspace_import("@scope/ui/widgets", &ws).as_deref(),
            Some("packages/ui/widgets")
        );
        assert_eq!(resolve_workspace_import("@scope/other", &ws), None);
        // Prefix must be a whole path segment: `@scope/ui-extra` ≠ `@scope/ui`.
        assert_eq!(resolve_workspace_import("@scope/ui-extra", &ws), None);
    }

    #[test]
    fn resolve_workspace_import_longest_name_wins() {
        let mut by_name = HashMap::new();
        by_name.insert("@scope/ui".to_string(), "packages/ui".to_string());
        by_name.insert("@scope/ui/core".to_string(), "packages/ui-core".to_string());
        let ws = WorkspacePackages { by_name };

        assert_eq!(
            resolve_workspace_import("@scope/ui/core", &ws).as_deref(),
            Some("packages/ui-core")
        );
        assert_eq!(
            resolve_workspace_import("@scope/ui/core/button", &ws).as_deref(),
            Some("packages/ui-core/button")
        );
        assert_eq!(
            resolve_workspace_import("@scope/ui/other", &ws).as_deref(),
            Some("packages/ui/other")
        );
    }

    #[test]
    fn pnpm_packages_block_ends_at_next_top_level_key() {
        let parsed = parse_pnpm_packages(
            "packages:\n  - 'packages/*'\nsomekey: value\n  - 'not/included'\n",
        );
        assert_eq!(parsed, vec!["packages/*".to_string()]);
    }
}

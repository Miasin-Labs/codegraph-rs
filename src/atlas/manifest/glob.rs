//! Workspace member globs (`crates/*`, `packages/**`, `apps/web`), expanded
//! one path segment at a time against the filesystem — bounded, never a
//! walk of the whole tree.

use std::fs;
use std::path::{Path, PathBuf};

use globset::{GlobBuilder, GlobMatcher};

/// Most directories one pattern may expand to.
const MAX_MATCHES: usize = 1_000;
/// How deep a `**` segment descends.
const MAX_GLOBSTAR_DEPTH: usize = 4;

/// Directories below `base` matching any of `includes` and none of
/// `excludes`, each holding `marker` (e.g. `Cargo.toml`), sorted, with the
/// include pattern that matched.
pub(super) fn expand(
    base: &Path,
    includes: &[&str],
    excludes: &[&str],
    marker: &str,
) -> Vec<(PathBuf, String)> {
    let excluded: Vec<GlobMatcher> = excludes
        .iter()
        .filter_map(|pattern| matcher(pattern.trim_start_matches("./").trim_end_matches('/')))
        .collect();
    let mut out: Vec<(PathBuf, String)> = Vec::new();
    for pattern in includes {
        let trimmed = pattern.trim_start_matches("./").trim_end_matches('/');
        for dir in expand_one(base, trimmed) {
            let rel = dir.strip_prefix(base).unwrap_or(&dir);
            let rel_text = rel.to_string_lossy().replace('\\', "/");
            if excluded.iter().any(|m| m.is_match(&rel_text)) || !dir.join(marker).is_file() {
                continue;
            }
            if !out.iter().any(|(seen, _)| *seen == dir) {
                out.push((dir, (*pattern).to_owned()));
            }
        }
    }
    out.sort();
    out
}

fn expand_one(base: &Path, pattern: &str) -> Vec<PathBuf> {
    if pattern.is_empty() || pattern == "." {
        return vec![base.to_path_buf()];
    }
    let mut current = vec![base.to_path_buf()];
    for segment in pattern.split('/').filter(|s| !s.is_empty() && *s != ".") {
        let mut next = Vec::new();
        for dir in &current {
            if segment == ".." {
                next.push(dir.parent().map_or_else(|| dir.clone(), Path::to_path_buf));
            } else if segment == "**" {
                globstar(dir, 0, &mut next);
            } else if segment.contains(['*', '?', '[', '{']) {
                let Some(m) = matcher(segment) else { continue };
                for child in child_dirs(dir) {
                    if child
                        .file_name()
                        .is_some_and(|name| m.is_match(name.to_string_lossy().as_ref()))
                    {
                        next.push(child);
                    }
                }
            } else {
                let child = dir.join(segment);
                if child.is_dir() {
                    next.push(child);
                }
            }
            if next.len() >= MAX_MATCHES {
                break;
            }
        }
        next.truncate(MAX_MATCHES);
        current = next;
    }
    current
}

/// `dir` and its descendants to [`MAX_GLOBSTAR_DEPTH`] (hidden and
/// dependency directories skipped).
fn globstar(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    out.push(dir.to_path_buf());
    if depth >= MAX_GLOBSTAR_DEPTH || out.len() >= MAX_MATCHES {
        return;
    }
    for child in child_dirs(dir) {
        globstar(&child, depth + 1, out);
    }
}

/// Real (non-symlink) subdirectories, minus hidden and dependency dirs.
fn child_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            !name.starts_with('.') && name != "node_modules" && name != "target"
        })
        .map(|e| e.path())
        .collect();
    dirs.sort();
    dirs
}

fn matcher(pattern: &str) -> Option<GlobMatcher> {
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .ok()
        .map(|g| g.compile_matcher())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(base: &Path, rel: &str, marker: &str) {
        fs::create_dir_all(base.join(rel)).unwrap();
        fs::write(base.join(rel).join(marker), "").unwrap();
    }

    #[test]
    fn globs_expand_segment_by_segment_and_honour_excludes() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        pkg(base, "crates/a", "Cargo.toml");
        pkg(base, "crates/b", "Cargo.toml");
        pkg(base, "crates/skip", "Cargo.toml");
        fs::create_dir_all(base.join("crates/no-manifest")).unwrap();
        pkg(base, "tools/deep/x/y", "Cargo.toml");
        pkg(base, "crates/node_modules/z", "Cargo.toml");
        let found = expand(
            base,
            &["crates/*", "tools/**", "./crates/a/"],
            &["crates/skip"],
            "Cargo.toml",
        );
        let rels: Vec<String> = found
            .iter()
            .map(|(d, _)| d.strip_prefix(base).unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(rels, ["crates/a", "crates/b", "tools/deep/x/y"]);
        assert_eq!(found[0].1, "crates/*");
    }

    #[test]
    fn parent_segments_and_literal_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        pkg(tmp.path(), "sibling", "package.json");
        fs::create_dir_all(&ws).unwrap();
        let found = expand(&ws, &["../sibling", "missing/*"], &[], "package.json");
        assert_eq!(found.len(), 1);
        assert!(found[0].0.ends_with("sibling"));
    }
}

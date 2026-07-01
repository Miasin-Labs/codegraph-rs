use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use super::git::get_git_visible_files;
use super::ignore::build_default_ignore;
use crate::error::log_debug;
use crate::extraction::grammars::is_source_file;
use crate::utils::{normalize_path, validate_existing_path_within_root_real};

/// Recursively scan a directory for source files.
///
/// In git repos, uses `git ls-files` (inherently respects .gitignore at all
/// levels), then keeps files with a supported source extension. For non-git
/// projects, falls back to a filesystem walk that parses .gitignore itself.
pub fn scan_directory(
    root_dir: &Path,
    mut on_progress: Option<&mut dyn FnMut(usize, &str)>,
) -> Vec<String> {
    // Fast path: use git to get all visible files (respects .gitignore everywhere)
    if let Some(git_files) = get_git_visible_files(root_dir) {
        let mut files: Vec<String> = Vec::new();
        let mut count = 0usize;
        for file_path in git_files {
            if is_source_file(&file_path)
                && validate_existing_path_within_root_real(root_dir, &file_path).is_some()
            {
                count += 1;
                if let Some(cb) = on_progress.as_deref_mut() {
                    cb(count, &file_path);
                }
                files.push(file_path);
            }
        }
        return files;
    }

    // Fallback: walk filesystem for non-git projects
    scan_directory_walk(root_dir, on_progress)
}

/// A .gitignore matcher scoped to the directory that declared it. Patterns in
/// a nested .gitignore are relative to that directory, so we keep the dir
/// alongside the matcher and test paths relative to it — mirroring how git
/// applies .gitignore files at every level.
struct ScopedIgnore {
    dir: PathBuf,
    ig: Gitignore,
}

fn load_ignore(dir: &Path) -> Option<ScopedIgnore> {
    let gi_path = dir.join(".gitignore");
    if gi_path.exists() {
        let mut builder = GitignoreBuilder::new(dir);
        let _ = builder.add(&gi_path);
        if let Ok(ig) = builder.build() {
            return Some(ScopedIgnore {
                dir: dir.to_path_buf(),
                ig,
            });
        }
    }
    // Unreadable .gitignore — treat as absent.
    None
}

fn is_ignored(full_path: &Path, is_dir: bool, matchers: &[ScopedIgnore]) -> bool {
    let mut ignored = false;
    for m in matchers {
        let rel = match full_path.strip_prefix(&m.dir) {
            Ok(r) => normalize_path(&r.to_string_lossy()),
            Err(_) => continue, // not under this matcher's dir
        };
        if rel.is_empty() {
            continue;
        }
        let matched = m.ig.matched_path_or_any_parents(&rel, is_dir);
        if matched.is_ignore() {
            ignored = true;
        } else if matched.is_whitelist() {
            ignored = false;
        }
    }
    ignored
}

/// Filesystem walk fallback for non-git projects.
pub(super) fn scan_directory_walk(
    root_dir: &Path,
    mut on_progress: Option<&mut dyn FnMut(usize, &str)>,
) -> Vec<String> {
    let mut files: Vec<String> = Vec::new();
    let mut count = 0usize;
    let mut visited_dirs: HashSet<PathBuf> = HashSet::new();

    #[allow(clippy::too_many_arguments)]
    fn walk(
        dir: &Path,
        root_dir: &Path,
        matchers: &mut Vec<ScopedIgnore>,
        visited_dirs: &mut HashSet<PathBuf>,
        files: &mut Vec<String>,
        count: &mut usize,
        on_progress: &mut Option<&mut dyn FnMut(usize, &str)>,
    ) {
        let real_dir = match fs::canonicalize(dir) {
            Ok(p) => p,
            Err(_) => {
                log_debug(
                    "Skipping unresolvable directory",
                    Some(&serde_json::json!({ "dir": dir.to_string_lossy() })),
                );
                return;
            }
        };

        if visited_dirs.contains(&real_dir) {
            log_debug(
                "Skipping already-visited directory (symlink cycle)",
                Some(&serde_json::json!({
                    "dir": dir.to_string_lossy(),
                    "realDir": real_dir.to_string_lossy(),
                })),
            );
            return;
        }
        visited_dirs.insert(real_dir);

        // This directory's own .gitignore (if present) applies to everything below it.
        // The root's .gitignore is already merged into the seeded base matcher (so a
        // negation there can override a built-in default), so skip it here.
        let own = if dir == root_dir {
            None
        } else {
            load_ignore(dir)
        };
        let pushed = own.is_some();
        if let Some(o) = own {
            matchers.push(o);
        }

        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(error) => {
                log_debug(
                    "Skipping unreadable directory",
                    Some(&serde_json::json!({
                        "dir": dir.to_string_lossy(),
                        "error": error.to_string(),
                    })),
                );
                if pushed {
                    matchers.pop();
                }
                return;
            }
        };

        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            // Never descend into git internals or our own data directory.
            if name_str == ".git" || name_str == ".codegraph" {
                continue;
            }

            let full_path = dir.join(&name);
            let relative_path = match full_path.strip_prefix(root_dir) {
                Ok(rel) => normalize_path(&rel.to_string_lossy()),
                Err(_) => normalize_path(&full_path.to_string_lossy()),
            };

            let file_type = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };

            if file_type.is_symlink() {
                if validate_existing_path_within_root_real(root_dir, &relative_path).is_none() {
                    log_debug(
                        "Skipping symlink outside project root",
                        Some(&serde_json::json!({ "path": full_path.to_string_lossy() })),
                    );
                    continue;
                }

                let resolved =
                    fs::canonicalize(&full_path).and_then(|target| fs::metadata(&target));
                match resolved {
                    Ok(stat) => {
                        if stat.is_dir() {
                            if !is_ignored(&full_path, true, matchers) {
                                walk(
                                    &full_path,
                                    root_dir,
                                    matchers,
                                    visited_dirs,
                                    files,
                                    count,
                                    on_progress,
                                );
                            }
                        } else if stat.is_file()
                            && !is_ignored(&full_path, false, matchers)
                            && is_source_file(&relative_path)
                        {
                            files.push(relative_path.clone());
                            *count += 1;
                            if let Some(cb) = on_progress.as_deref_mut() {
                                cb(*count, &relative_path);
                            }
                        }
                    }
                    Err(_) => {
                        log_debug(
                            "Skipping broken symlink",
                            Some(&serde_json::json!({ "path": full_path.to_string_lossy() })),
                        );
                    }
                }
                continue;
            }

            if file_type.is_dir() {
                if !is_ignored(&full_path, true, matchers) {
                    walk(
                        &full_path,
                        root_dir,
                        matchers,
                        visited_dirs,
                        files,
                        count,
                        on_progress,
                    );
                }
            } else if file_type.is_file()
                && !is_ignored(&full_path, false, matchers)
                && is_source_file(&relative_path)
            {
                files.push(relative_path.clone());
                *count += 1;
                if let Some(cb) = on_progress.as_deref_mut() {
                    cb(*count, &relative_path);
                }
            }
        }

        if pushed {
            matchers.pop();
        }
    }

    // Seed a base matcher with the built-in default ignores (merged with the root
    // .gitignore so a negation can override). Nested .gitignores still layer per-dir.
    let mut matchers = vec![ScopedIgnore {
        dir: root_dir.to_path_buf(),
        ig: build_default_ignore(root_dir),
    }];
    walk(
        root_dir,
        root_dir,
        &mut matchers,
        &mut visited_dirs,
        &mut files,
        &mut count,
        &mut on_progress,
    );
    files
}

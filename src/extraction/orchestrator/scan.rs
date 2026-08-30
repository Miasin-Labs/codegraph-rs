use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use super::git::get_git_visible_files;
use super::ignore::{build_default_ignore, build_defaults_only_ignore, gitignore_ignores};
use crate::directory::is_codegraph_data_dir;
use crate::error::log_debug;
use crate::extraction::file_selection::is_indexable_existing_file;
use crate::project_config::{ProjectConfig, load_project_config, matcher_matches};
use crate::utils::{normalize_path, validate_existing_path_within_root_real};

/// What a scan saw but could not index, tallied by extension.
///
/// Filled during the walk the scan already performs. A project written entirely
/// in a language CodeGraph has no grammar for is otherwise indistinguishable
/// from an empty one — unsupported extensions are filtered out at discovery and
/// counted nowhere, so `index`/`init` report "No files found" with a `complete`
/// index state and exit 0, telling neither a human nor an agent that there was
/// code here it could not read (issue #1502 / refPR #1605).
#[derive(Debug, Clone, Default)]
pub struct ScanSkipStats {
    /// Lowercased extension (with dot) → how many files carried it. A `Vec`
    /// (not a map) preserves first-seen order, mirroring the TS `Map`.
    pub unsupported_by_extension: Vec<(String, usize)>,
}

impl ScanSkipStats {
    /// Record one file the scan declined to index. Files without an extension
    /// are ignored, matching the TS `tallySkip` (`path.extname` empty → skip).
    fn tally(&mut self, relative_path: &str) {
        let name = relative_path.rsplit('/').next().unwrap_or(relative_path);
        let Some(dot) = name.rfind('.') else {
            return;
        };
        // A leading dot (dotfile with no other dot) has no extension in Node's
        // `path.extname` sense — `.gitignore` → "".
        if dot == 0 {
            return;
        }
        let ext = name[dot..].to_ascii_lowercase();
        match self
            .unsupported_by_extension
            .iter_mut()
            .find(|(e, _)| *e == ext)
        {
            Some((_, count)) => *count += 1,
            None => self.unsupported_by_extension.push((ext, 1)),
        }
    }
}

/// Record one skipped file into the optional stats sink.
fn tally_skip(stats: &mut Option<&mut ScanSkipStats>, relative_path: &str) {
    if let Some(s) = stats.as_deref_mut() {
        s.tally(relative_path);
    }
}

/// Recursively scan a directory for source files.
///
/// In git repos, uses `git ls-files` (inherently respects .gitignore at all
/// levels), then keeps files with a supported source extension. For non-git
/// projects, falls back to a filesystem walk that parses .gitignore itself.
pub fn scan_directory(
    root_dir: &Path,
    on_progress: Option<&mut dyn FnMut(usize, &str)>,
) -> Vec<String> {
    let config = load_project_config(root_dir);
    scan_directory_with_config(root_dir, on_progress, &config)
}

pub(super) fn scan_directory_with_config(
    root_dir: &Path,
    on_progress: Option<&mut dyn FnMut(usize, &str)>,
    config: &ProjectConfig,
) -> Vec<String> {
    scan_directory_with_config_stats(root_dir, on_progress, config, None)
}

/// Like [`scan_directory_with_config`] but also tallies files the scan saw yet
/// has no grammar for, into `stats` (issue #1502).
pub(super) fn scan_directory_with_config_stats(
    root_dir: &Path,
    mut on_progress: Option<&mut dyn FnMut(usize, &str)>,
    config: &ProjectConfig,
    mut stats: Option<&mut ScanSkipStats>,
) -> Vec<String> {
    let exclude = config.exclude_matcher(root_dir);
    // Fast path: use git to get all visible files (respects .gitignore everywhere)
    if let Some(git_files) = get_git_visible_files(root_dir, config) {
        let mut files: Vec<String> = Vec::new();
        let mut seen = HashSet::new();
        let mut count = 0usize;
        for file_path in git_files {
            if matcher_matches(exclude.as_ref(), &file_path, false) {
                continue;
            }
            if is_indexable_existing_file(root_dir, &file_path, config.extension_overrides()) {
                if seen.insert(file_path.clone()) {
                    count += 1;
                    if let Some(cb) = on_progress.as_deref_mut() {
                        cb(count, &file_path);
                    }
                    files.push(file_path);
                }
            } else {
                // Visible to git and not excluded, but no grammar for it — count
                // it so an all-unsupported project isn't reported as empty (#1502).
                tally_skip(&mut stats, &file_path);
            }
        }
        append_included_files(
            root_dir,
            config,
            &mut files,
            &mut seen,
            &mut count,
            &mut on_progress,
        );
        return files;
    }

    // Fallback: walk filesystem for non-git projects
    scan_directory_walk_with_config(root_dir, on_progress, config, stats)
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

fn scan_directory_walk_with_config(
    root_dir: &Path,
    mut on_progress: Option<&mut dyn FnMut(usize, &str)>,
    config: &ProjectConfig,
    mut stats: Option<&mut ScanSkipStats>,
) -> Vec<String> {
    let mut files: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut count = 0usize;
    let mut visited_dirs: HashSet<PathBuf> = HashSet::new();
    let exclude = config.exclude_matcher(root_dir);

    #[allow(clippy::too_many_arguments)]
    fn walk(
        dir: &Path,
        root_dir: &Path,
        matchers: &mut Vec<ScopedIgnore>,
        exclude: Option<&Gitignore>,
        config: &ProjectConfig,
        visited_dirs: &mut HashSet<PathBuf>,
        files: &mut Vec<String>,
        seen: &mut HashSet<String>,
        count: &mut usize,
        on_progress: &mut Option<&mut dyn FnMut(usize, &str)>,
        stats: &mut Option<&mut ScanSkipStats>,
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
            if name_str == ".git" || is_codegraph_data_dir(&name_str) {
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
            let config_excluded = matcher_matches(exclude, &relative_path, file_type.is_dir());
            if config_excluded {
                continue;
            }

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
                                    exclude,
                                    config,
                                    visited_dirs,
                                    files,
                                    seen,
                                    count,
                                    on_progress,
                                    stats,
                                );
                            }
                        } else if stat.is_file() && !is_ignored(&full_path, false, matchers) {
                            if is_indexable_existing_file(
                                root_dir,
                                &relative_path,
                                config.extension_overrides(),
                            ) {
                                if seen.insert(relative_path.clone()) {
                                    files.push(relative_path.clone());
                                    *count += 1;
                                    if let Some(cb) = on_progress.as_deref_mut() {
                                        cb(*count, &relative_path);
                                    }
                                }
                            } else {
                                tally_skip(stats, &relative_path);
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
                        exclude,
                        config,
                        visited_dirs,
                        files,
                        seen,
                        count,
                        on_progress,
                        stats,
                    );
                }
            } else if file_type.is_file() && !is_ignored(&full_path, false, matchers) {
                if is_indexable_existing_file(
                    root_dir,
                    &relative_path,
                    config.extension_overrides(),
                ) {
                    if seen.insert(relative_path.clone()) {
                        files.push(relative_path.clone());
                        *count += 1;
                        if let Some(cb) = on_progress.as_deref_mut() {
                            cb(*count, &relative_path);
                        }
                    }
                } else {
                    tally_skip(stats, &relative_path);
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
        exclude.as_ref(),
        config,
        &mut visited_dirs,
        &mut files,
        &mut seen,
        &mut count,
        &mut on_progress,
        &mut stats,
    );
    append_included_files(
        root_dir,
        config,
        &mut files,
        &mut seen,
        &mut count,
        &mut on_progress,
    );
    files
}

fn append_included_files(
    root_dir: &Path,
    config: &ProjectConfig,
    files: &mut Vec<String>,
    seen: &mut HashSet<String>,
    count: &mut usize,
    on_progress: &mut Option<&mut dyn FnMut(usize, &str)>,
) {
    for file_path in collect_included_files(root_dir, config) {
        if seen.insert(file_path.clone()) {
            files.push(file_path.clone());
            *count += 1;
            if let Some(callback) = on_progress.as_deref_mut() {
                callback(*count, &file_path);
            }
        }
    }
}

/// Find files force-included despite `.gitignore`. Built-in dependency/build
/// directories, `.git`, CodeGraph data dirs, and explicit excludes still win.
fn collect_included_files(root_dir: &Path, config: &ProjectConfig) -> Vec<String> {
    let Some(include) = config.include_matcher(root_dir) else {
        return Vec::new();
    };
    let exclude = config.exclude_matcher(root_dir);
    let defaults = build_defaults_only_ignore(root_dir);
    let mut files = Vec::new();
    let mut visited = HashSet::new();

    fn walk(
        dir: &Path,
        root_dir: &Path,
        config: &ProjectConfig,
        include: &Gitignore,
        exclude: Option<&Gitignore>,
        defaults: &Gitignore,
        visited: &mut HashSet<PathBuf>,
        files: &mut Vec<String>,
    ) {
        let Ok(real_dir) = fs::canonicalize(dir) else {
            return;
        };
        if !real_dir.starts_with(root_dir) || !visited.insert(real_dir) {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == ".git" || is_codegraph_data_dir(&name) {
                continue;
            }
            let full_path = entry.path();
            let Ok(relative) = full_path.strip_prefix(root_dir) else {
                continue;
            };
            let relative = normalize_path(&relative.to_string_lossy());
            if matcher_matches(exclude, &relative, file_type.is_dir()) {
                continue;
            }
            if file_type.is_dir() {
                if !gitignore_ignores(defaults, &relative, true) {
                    walk(
                        &full_path, root_dir, config, include, exclude, defaults, visited, files,
                    );
                }
            } else if file_type.is_file()
                && matcher_matches(Some(include), &relative, false)
                && is_indexable_existing_file(root_dir, &relative, config.extension_overrides())
            {
                files.push(relative);
            }
        }
    }

    let canonical_root = fs::canonicalize(root_dir).unwrap_or_else(|_| root_dir.to_path_buf());
    walk(
        &canonical_root,
        &canonical_root,
        config,
        &include,
        exclude.as_ref(),
        &defaults,
        &mut visited,
        &mut files,
    );
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};

    use super::*;

    fn git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("git should be runnable");
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn git_scan_skips_submodule_gitlink_directories_with_source_extensions() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("third_party/llama.cpp")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();

        git(root, &["init", "-q"]);
        git(root, &["add", "src/main.rs"]);
        git(
            root,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                "160000,a94e6ff8774b7c9f950d9545baf0ce35e8d1ed2f,third_party/llama.cpp",
            ],
        );

        let files = scan_directory_with_config(root, None, &ProjectConfig::default());

        assert!(files.iter().any(|file| file == "src/main.rs"), "{files:?}");
        assert!(
            !files.iter().any(|file| file == "third_party/llama.cpp"),
            "gitlinks that materialize as directories are not readable source files: {files:?}"
        );
    }

    #[test]
    fn scan_tallies_unsupported_extensions_on_the_git_path() {
        // A project of only unsupported files must not look empty: the scan
        // records what it declined to index, by extension (#1502).
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::write(root.join("a.pl"), "print 1;\n").unwrap();
        fs::write(root.join("b.pl"), "print 2;\n").unwrap();
        fs::write(root.join("c.foobar"), "x\n").unwrap();
        git(root, &["init", "-q"]);
        git(root, &["add", "a.pl", "b.pl", "c.foobar"]);

        let mut stats = ScanSkipStats::default();
        let files = scan_directory_with_config_stats(
            root,
            None,
            &ProjectConfig::default(),
            Some(&mut stats),
        );

        assert!(files.is_empty(), "no file should be indexable: {files:?}");
        let by_ext: std::collections::HashMap<_, _> =
            stats.unsupported_by_extension.iter().cloned().collect();
        assert_eq!(by_ext.get(".pl"), Some(&2));
        assert_eq!(by_ext.get(".foobar"), Some(&1));
    }

    #[test]
    fn scan_tallies_unsupported_extensions_on_the_filesystem_walk_path() {
        // Same tally on the non-git fallback (no `.git` here), because the two
        // discovery paths filter in different places (#1502).
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::write(root.join("a.pl"), "print 1;\n").unwrap();
        fs::write(root.join("b.foobar"), "x\n").unwrap();

        let mut stats = ScanSkipStats::default();
        let files = scan_directory_with_config_stats(
            root,
            None,
            &ProjectConfig::default(),
            Some(&mut stats),
        );

        assert!(files.is_empty(), "no file should be indexable: {files:?}");
        let by_ext: std::collections::HashMap<_, _> =
            stats.unsupported_by_extension.iter().cloned().collect();
        assert_eq!(by_ext.get(".pl"), Some(&1));
        assert_eq!(by_ext.get(".foobar"), Some(&1));
    }

    #[test]
    fn scan_records_no_skips_when_every_file_is_indexable() {
        // The counter must stay empty on a healthy project so it never fires on
        // the happy path (#1502).
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::write(root.join("a.rs"), "fn a() {}\n").unwrap();
        fs::write(root.join("b.ts"), "export const b = 1;\n").unwrap();

        let mut stats = ScanSkipStats::default();
        let files = scan_directory_with_config_stats(
            root,
            None,
            &ProjectConfig::default(),
            Some(&mut stats),
        );

        assert_eq!(files.len(), 2, "both files should index: {files:?}");
        assert!(
            stats.unsupported_by_extension.is_empty(),
            "no skips expected: {:?}",
            stats.unsupported_by_extension
        );
    }
}

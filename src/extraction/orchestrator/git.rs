use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::ignore::{build_default_ignore, build_defaults_only_ignore, gitignore_ignores};
use crate::directory::is_codegraph_data_dir;
use crate::project_config::{ProjectConfig, matcher_matches};
use crate::utils::normalize_path;

// =============================================================================
// git enumeration
// =============================================================================

pub(super) struct GitOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
}

/// Run `git <args>` in `cwd` with a wall-clock timeout (the TS `execFileSync`
/// options: piped stdio, timeout). Returns `None` on spawn failure or timeout
/// (TS: throws → caught by callers).
pub(super) fn run_git(cwd: &Path, args: &[&str], timeout: Duration) -> Option<GitOutput> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;

    // Drain pipes on background threads so a chatty git can't deadlock on a
    // full pipe while we wait.
    let mut stdout_pipe = child.stdout.take()?;
    let stdout_handle = std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf);
        buf
    });
    if let Some(mut stderr_pipe) = child.stderr.take() {
        std::thread::spawn(move || {
            use std::io::Read;
            let mut sink = Vec::new();
            let _ = stderr_pipe.read_to_end(&mut sink);
        });
    }

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return None,
        }
    };
    let stdout = stdout_handle.join().ok()?;
    Some(GitOutput { status, stdout })
}

/// Collect git-visible files (tracked + untracked, .gitignore-respected) from the
/// git repository rooted at `repo_dir`, adding each to `files` with `prefix`
/// prepended so paths stay relative to the original scan root.
///
/// Recurses into embedded git repositories — nested repos that are NOT submodules
/// (independent clones living inside the workspace, common in CMake "super-repo"
/// layouts). The parent repo's `git ls-files` cannot see into them: tracked output
/// skips them entirely, and untracked output reports them only as an opaque
/// "subdir/" entry (trailing slash) rather than expanding their files. Each
/// embedded repo is its own git boundary, so we re-run `git ls-files` inside it.
/// (See issue #193.)
///
/// Returns `None` on any git failure (TS: the throw propagated to
/// `getGitVisibleFiles`' catch).
pub(super) fn collect_git_files(
    repo_dir: &Path,
    prefix: &str,
    files: &mut Vec<String>,
    seen: &mut HashSet<String>,
) -> Option<()> {
    let timeout = Duration::from_millis(30_000);

    // Tracked files. --recurse-submodules pulls in files from active submodules,
    // which the index would otherwise represent only as a commit pointer.
    // Without this, monorepos using submodules index 0 files. (See issue #147.)
    // Note: --recurse-submodules only supports -c/--cached and --stage modes — it
    // can't be combined with -o, so untracked files are gathered separately below.
    // -z gives NUL-separated, unquoted output so non-ASCII (e.g. CJK) paths
    // survive verbatim. Without it git octal-escapes and double-quotes such paths
    // (the core.quotepath default), and the quoted form never matches a real file
    // on disk → those files are silently dropped from the index. (#541)
    let tracked = run_git(
        repo_dir,
        &["ls-files", "-z", "-c", "--recurse-submodules"],
        timeout,
    )?;
    if !tracked.status.success() {
        return None;
    }
    for rel in tracked.stdout.split(|b| *b == 0) {
        if rel.is_empty() {
            continue;
        }
        let rel = String::from_utf8_lossy(rel);
        let normalized = normalize_path(&format!("{prefix}{rel}"));
        if seen.insert(normalized.clone()) {
            files.push(normalized);
        }
    }

    // Untracked files (submodules manage their own untracked state). Embedded git
    // repos surface here as a single "subdir/" entry that git refuses to descend
    // into — recurse into those as their own repos so their source gets indexed.
    let untracked = run_git(
        repo_dir,
        &["ls-files", "-z", "-o", "--exclude-standard"],
        timeout,
    )?;
    if !untracked.status.success() {
        return None;
    }
    for rel in untracked.stdout.split(|b| *b == 0) {
        if rel.is_empty() {
            continue;
        }
        let rel = String::from_utf8_lossy(rel).into_owned();
        if rel.ends_with('/') {
            // git only emits a trailing-slash directory entry for an embedded repo.
            // Guard with a .git check anyway, and skip anything else exactly as git
            // itself skips it (we never descend into a non-repo opaque dir).
            let child_dir = repo_dir.join(&rel);
            if child_dir.join(".git").exists() {
                collect_git_files(&child_dir, &format!("{prefix}{rel}"), files, seen)?;
            }
            continue;
        }
        let normalized = normalize_path(&format!("{prefix}{rel}"));
        if seen.insert(normalized.clone()) {
            files.push(normalized);
        }
    }
    Some(())
}

/// Get all files visible to git (tracked + untracked but not ignored).
/// Respects .gitignore at all levels (root, subdirectories) and descends into
/// embedded (nested, non-submodule) git repos. Returns `None` on failure
/// (non-git project) so callers can fall back to a filesystem walk.
pub(super) fn get_git_visible_files(
    root_dir: &Path,
    config: &ProjectConfig,
) -> Option<Vec<String>> {
    let short = Duration::from_millis(5_000);

    // Check if the project directory is gitignored by a parent repo.
    // When rootDir lives inside a parent git repo that ignores it,
    // `git ls-files` returns nothing — fall back to filesystem walk.
    let out = run_git(root_dir, &["rev-parse", "--show-toplevel"], short)?;
    if !out.status.success() {
        return None;
    }
    let git_root = String::from_utf8_lossy(&out.stdout).trim().to_string();

    let git_root_resolved =
        fs::canonicalize(&git_root).unwrap_or_else(|_| PathBuf::from(&git_root));
    let root_resolved = fs::canonicalize(root_dir).unwrap_or_else(|_| root_dir.to_path_buf());
    if git_root_resolved != root_resolved {
        // git check-ignore exits 0 if the path IS ignored, 1 if not
        let abs_root = root_resolved.to_string_lossy().into_owned();
        if let Some(check) = run_git(root_dir, &["check-ignore", "-q", &abs_root], short) {
            if check.status.success() {
                // Directory is gitignored by parent repo — fall back to filesystem walk
                return None;
            }
        }
        // Not ignored — safe to use git ls-files
    }

    let mut files = Vec::new();
    let mut seen = HashSet::new();
    collect_git_files(root_dir, "", &mut files, &mut seen)?;
    let mut opted_in_files = HashSet::new();
    collect_opted_in_embedded_repos(root_dir, config, &mut files, &mut seen, &mut opted_in_files);
    // Apply built-in default ignores uniformly — to tracked files too, since
    // committing a dependency/build dir doesn't make it project code. A
    // `.gitignore` negation (e.g. `!vendor/`) is the explicit opt-in. (issue #407)
    let ig = build_default_ignore(root_dir);
    let defaults = build_defaults_only_ignore(root_dir);
    Some(
        files
            .into_iter()
            .filter(|file| {
                if opted_in_files.contains(file) {
                    !gitignore_ignores(&defaults, file, false)
                } else {
                    !gitignore_ignores(&ig, file, false)
                }
            })
            .collect(),
    )
}

/// Discover embedded repositories hidden by `.gitignore` only when the project
/// explicitly opts their containing path in through `includeIgnored`.
fn collect_opted_in_embedded_repos(
    root_dir: &Path,
    config: &ProjectConfig,
    files: &mut Vec<String>,
    seen_files: &mut HashSet<String>,
    opted_in_files: &mut HashSet<String>,
) {
    let Some(include) = config.include_ignored_matcher(root_dir) else {
        return;
    };
    let defaults = build_defaults_only_ignore(root_dir);
    let mut visited = HashSet::new();

    fn walk(
        dir: &Path,
        root_dir: &Path,
        include: &ignore::gitignore::Gitignore,
        defaults: &ignore::gitignore::Gitignore,
        visited: &mut HashSet<PathBuf>,
        files: &mut Vec<String>,
        seen_files: &mut HashSet<String>,
        opted_in_files: &mut HashSet<String>,
    ) {
        let real = match fs::canonicalize(dir) {
            Ok(real) if real.starts_with(root_dir) || real == root_dir => real,
            _ => return,
        };
        if !visited.insert(real) {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() || file_type.is_symlink() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == ".git" || is_codegraph_data_dir(&name) {
                continue;
            }
            let path = entry.path();
            let Ok(relative) = path.strip_prefix(root_dir) else {
                continue;
            };
            let mut relative = normalize_path(&relative.to_string_lossy());
            relative.push('/');
            if gitignore_ignores(defaults, &relative, true) {
                continue;
            }

            if path.join(".git").exists() && matcher_matches(Some(include), &relative, true) {
                let before = files.len();
                let _ = collect_git_files(&path, &relative, files, seen_files);
                opted_in_files.extend(files[before..].iter().cloned());
            }
            walk(
                &path,
                root_dir,
                include,
                defaults,
                visited,
                files,
                seen_files,
                opted_in_files,
            );
        }
    }

    let canonical_root = fs::canonicalize(root_dir).unwrap_or_else(|_| root_dir.to_path_buf());
    walk(
        &canonical_root,
        &canonical_root,
        &include,
        &defaults,
        &mut visited,
        files,
        seen_files,
        opted_in_files,
    );
}

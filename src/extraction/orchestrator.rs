//! Extraction Orchestrator
//!
//! Coordinates file scanning, parsing, and database storage.
//!
//! Ported from `src/extraction/index.ts`, plus the `extractFromSource`
//! dispatcher from the bottom of `src/extraction/tree-sitter.ts` (deferred to
//! this file by the extraction-core port because the standalone extractors it
//! routes to were concurrent stubs at that time — see
//! `notes/extraction-core.md`).
//!
//! Node-isms dropped (documented in `notes/extraction-orchestrator.md`):
//! - `parse-worker.ts` and the whole worker lifecycle (spawn/recycle/timeout,
//!   `PARSE_TIMEOUT_MS`, `WORKER_RECYCLE_INTERVAL`) — parsing is native and
//!   in-process; parallelism is rayon over read batches instead.
//! - The WASM memory-corruption retry pass (fresh-worker retry + comment
//!   stripping) — there is no WASM heap to corrupt.
//! - `scanDirectoryAsync` — only existed to yield to the Node event loop;
//!   [`scan_directory`] covers both call sites.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::db::QueryBuilder;
use crate::error::{Result, log_debug, log_warn};
use crate::extraction::dfm_extractor::DfmExtractor;
use crate::extraction::grammars::{
    detect_language,
    init_grammars,
    is_file_level_only_language,
    is_language_supported,
    is_source_file,
    load_grammars_for_languages,
};
use crate::extraction::ida_c_extractor::{IdaCExtractor, is_ida_generated_c};
use crate::extraction::languages;
use crate::extraction::liquid_extractor::LiquidExtractor;
use crate::extraction::mybatis_extractor::MyBatisExtractor;
use crate::extraction::svelte_extractor::SvelteExtractor;
use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
use crate::extraction::vue_extractor::VueExtractor;
use crate::resolution::frameworks::{
    detect_frameworks,
    get_all_framework_resolvers,
    get_applicable_frameworks,
};
use crate::resolution::types::{ImportMapping, ResolutionContext};
use crate::types::{
    ExtractionError,
    ExtractionResult,
    FileRecord,
    Language,
    Node,
    NodeKind,
    Severity,
    UnresolvedReference,
};
use crate::utils::{normalize_path, sha256_hex, validate_path_within_root};

/// Number of files to read in parallel during indexing.
/// File reads are I/O-bound; batching overlaps I/O wait with CPU parse work.
const FILE_IO_BATCH_SIZE: usize = 10;

/// Epoch milliseconds (`Date.now()` parity).
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Progress phase for indexing operations (TS string union).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexPhase {
    Scanning,
    Parsing,
    Storing,
    Resolving,
}

impl IndexPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            IndexPhase::Scanning => "scanning",
            IndexPhase::Parsing => "parsing",
            IndexPhase::Storing => "storing",
            IndexPhase::Resolving => "resolving",
        }
    }
}

/// Progress callback payload for indexing operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexProgress {
    pub phase: IndexPhase,
    pub current: usize,
    pub total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_file: Option<String>,
}

/// Result of an indexing operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexResult {
    pub success: bool,
    pub files_indexed: usize,
    pub files_skipped: usize,
    pub files_errored: usize,
    pub nodes_created: usize,
    pub edges_created: usize,
    pub errors: Vec<ExtractionError>,
    pub duration_ms: i64,
}

/// Result of a sync operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncResult {
    pub files_checked: usize,
    pub files_added: usize,
    pub files_modified: usize,
    pub files_removed: usize,
    pub nodes_updated: usize,
    pub duration_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_file_paths: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_node_names: Option<Vec<String>>,
}

/// Return shape of [`ExtractionOrchestrator::get_changed_files`]
/// (anonymous object in TS).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangedFiles {
    pub added: Vec<String>,
    pub modified: Vec<String>,
    pub removed: Vec<String>,
}

/// Return shape of [`ExtractionOrchestrator::reconcile_removed_files`]
/// (anonymous object in TS).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconcileResult {
    pub files_removed: usize,
    pub removed_node_names: Vec<String>,
}

/// The subset of `fs.Stats` the orchestrator consumes (size + mtime).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStats {
    /// File size in bytes (`stats.size`).
    pub size: u64,
    /// Modification time in epoch milliseconds (`stats.mtimeMs`, floored).
    pub modified_at_ms: i64,
}

impl FileStats {
    pub fn from_metadata(meta: &fs::Metadata) -> Self {
        FileStats {
            size: meta.len(),
            modified_at_ms: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
        }
    }
}

/// Calculate SHA256 hash of file contents.
pub fn hash_content(content: &str) -> String {
    sha256_hex(content.as_bytes())
}

/// Skip files larger than this (bytes). Generated bundles, minified JS, and
/// vendored blobs produce no useful symbols. 1 MB covers essentially all
/// hand-written source.
const MAX_FILE_SIZE: u64 = 1024 * 1024;
const MAX_IDA_FILE_SIZE: u64 = 5 * 1024 * 1024;

/// Directory names that are dependency, build, cache, or tooling output across the
/// languages/frameworks CodeGraph supports — curated from the canonical
/// github/gitignore templates. Excluded by default so the graph reflects your code,
/// not third-party noise, without requiring a `.gitignore` (issue #407). The
/// exclusion applies uniformly (git or not, tracked or not); the only opt-in is an
/// explicit `.gitignore` negation (e.g. `!vendor/`). First-party-prone or generic
/// names (`packages`, `lib`, `app`, `bin`, `src`, `deps`, `env`, `tmp`, `storage`,
/// `Library`) are deliberately NOT listed, to avoid ever hiding real source.
///
/// Only dirs that actually contain *indexable source* (or are enormous) earn a slot
/// — IDE/state dirs like `.idea`/`.vs` are omitted because CodeGraph indexes only
/// recognized source extensions, so they produce no symbols regardless.
const DEFAULT_IGNORE_DIRS: &[&str] = &[
    // JS / TS — dependency directories
    "node_modules",
    "bower_components",
    "jspm_packages",
    "web_modules",
    ".yarn",
    ".pnpm-store",
    // JS / TS — framework & bundler build / cache / deploy output
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".turbo",
    ".vite",
    ".parcel-cache",
    ".angular",
    ".docusaurus",
    "storybook-static",
    ".vinxi",
    ".nitro",
    "out-tsc",
    ".vercel",
    ".netlify",
    ".wrangler",
    // Build output (common across ecosystems)
    "dist",
    "build",
    "out",
    ".output",
    // Test / coverage
    "coverage",
    ".nyc_output",
    // Python
    "__pycache__",
    "__pypackages__",
    ".venv",
    "venv",
    ".pixi",
    ".pdm-build",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".tox",
    ".nox",
    ".hypothesis",
    ".ipynb_checkpoints",
    ".eggs",
    // Rust / JVM (Maven, Gradle, Scala)
    "target",
    ".gradle",
    // .NET
    "obj",
    // Vendored deps (Go, PHP/Composer, Ruby/Bundler)
    "vendor",
    // Swift / iOS
    ".build",
    "Pods",
    "Carthage",
    "DerivedData",
    ".swiftpm",
    // Dart / Flutter
    ".dart_tool",
    ".pub-cache",
    // Native (Android NDK, C/C++ deps)
    ".cxx",
    ".externalNativeBuild",
    "vcpkg_installed",
    // Scala tooling
    ".bloop",
    ".metals",
    // Lua / Luau (LuaRocks)
    "lua_modules",
    ".luarocks",
    // Delphi / RAD Studio IDE backups (duplicate .pas source — would double-count)
    "__history",
    "__recovery",
    // Generic cache
    ".cache",
];

/// Gitignore-style patterns for the ignore matcher: the dirs above plus a few globs.
fn default_ignore_patterns() -> Vec<String> {
    let mut patterns: Vec<String> = DEFAULT_IGNORE_DIRS
        .iter()
        .map(|d| format!("{d}/"))
        .collect();
    patterns.push("*.egg-info/".to_string()); // Python packaging metadata
    patterns.push("cmake-build-*/".to_string()); // CLion / CMake build trees
    patterns.push("bazel-*/".to_string()); // Bazel output symlink trees
    patterns
}

/// An ignore matcher seeded with the built-in defaults, merged with the project's
/// root .gitignore and .codegraphignore. `.codegraphignore` is intentionally
/// CodeGraph-only: it can exclude tracked reference/generated corpora that should
/// remain in git but should not pollute the symbol graph. Shared by both
/// enumeration paths so behavior is identical with or without git — and so the
/// defaults apply to tracked files too (committing a dependency dir doesn't make
/// it project code; an explicit negation remains the opt-in).
pub fn build_default_ignore(root_dir: &Path) -> Gitignore {
    let mut builder = GitignoreBuilder::new(root_dir);
    for pattern in default_ignore_patterns() {
        let _ = builder.add_line(None, &pattern);
    }
    for file_name in [".gitignore", ".codegraphignore"] {
        let ignore_path = root_dir.join(file_name);
        if ignore_path.exists() {
            // Unreadable/partially-bad ignore file — the built-in defaults
            // still apply (errors per line are skipped by the builder).
            let _ = builder.add(&ignore_path);
        }
    }
    builder.build().unwrap_or_else(|_| Gitignore::empty())
}

/// `npm ignore`-pkg `.ignores()` parity: a path is ignored when the path or any
/// of its ancestor directories matches an ignore rule (negations re-include).
fn gitignore_ignores(ig: &Gitignore, rel_path: &str, is_dir: bool) -> bool {
    if rel_path.is_empty() {
        return false;
    }
    ig.matched_path_or_any_parents(rel_path, is_dir).is_ignore()
}

// =============================================================================
// git enumeration
// =============================================================================

struct GitOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
}

/// Run `git <args>` in `cwd` with a wall-clock timeout (the TS `execFileSync`
/// options: piped stdio, timeout). Returns `None` on spawn failure or timeout
/// (TS: throws → caught by callers).
fn run_git(cwd: &Path, args: &[&str], timeout: Duration) -> Option<GitOutput> {
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
fn collect_git_files(
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
fn get_git_visible_files(root_dir: &Path) -> Option<Vec<String>> {
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
    // Apply built-in default ignores uniformly — to tracked files too, since
    // committing a dependency/build dir doesn't make it project code. A
    // `.gitignore` negation (e.g. `!vendor/`) is the explicit opt-in. (issue #407)
    let ig = build_default_ignore(root_dir);
    Some(
        files
            .into_iter()
            .filter(|f| !gitignore_ignores(&ig, f, false))
            .collect(),
    )
}

// =============================================================================
// Directory scanning
// =============================================================================

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
            if is_source_file(&file_path) {
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
    for m in matchers {
        let rel = match full_path.strip_prefix(&m.dir) {
            Ok(r) => normalize_path(&r.to_string_lossy()),
            Err(_) => continue, // not under this matcher's dir
        };
        if rel.is_empty() {
            continue;
        }
        if gitignore_ignores(&m.ig, &rel, is_dir) {
            return true;
        }
    }
    false
}

/// Filesystem walk fallback for non-git projects.
fn scan_directory_walk(
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

// =============================================================================
// Filesystem-vs-index diff
// =============================================================================

struct FileDiffResult {
    files_checked: usize,
    added: Vec<String>,
    modified: Vec<String>,
    removed: Vec<String>,
}

/// Compare the current filesystem against the DB's tracked file state.
/// This is the freshness source of truth: git status is not enough because
/// pull/checkout/merge can change files and leave a clean working tree.
fn diff_filesystem_against_index(
    root_dir: &Path,
    queries: &QueryBuilder,
) -> Result<FileDiffResult> {
    let current_files = scan_directory(root_dir, None);
    let current_set: HashSet<&str> = current_files.iter().map(|s| s.as_str()).collect();
    let tracked_files = queries.get_all_files()?;
    let tracked_map: HashMap<&str, &FileRecord> =
        tracked_files.iter().map(|f| (f.path.as_str(), f)).collect();

    let mut added: Vec<String> = Vec::new();
    let mut modified: Vec<String> = Vec::new();
    let mut removed: Vec<String> = Vec::new();

    for tracked in &tracked_files {
        if !current_set.contains(tracked.path.as_str()) || !root_dir.join(&tracked.path).exists() {
            removed.push(tracked.path.clone());
        }
    }

    for file_path in &current_files {
        let full_path = root_dir.join(file_path);
        let tracked = tracked_map.get(file_path.as_str()).copied();

        if let Some(tracked) = tracked {
            match fs::metadata(&full_path) {
                Ok(stat) => {
                    let stats = FileStats::from_metadata(&stat);
                    if stats.size == tracked.size && stats.modified_at_ms == tracked.modified_at {
                        continue;
                    }
                }
                Err(error) => {
                    log_debug(
                        "Skipping unstattable file while detecting changes",
                        Some(&serde_json::json!({
                            "filePath": file_path,
                            "error": error.to_string(),
                        })),
                    );
                    continue;
                }
            }
        }

        let content = match fs::read(&full_path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(error) => {
                log_debug(
                    "Skipping unreadable file while detecting changes",
                    Some(&serde_json::json!({
                        "filePath": file_path,
                        "error": error.to_string(),
                    })),
                );
                continue;
            }
        };

        let content_hash = hash_content(&content);
        match tracked {
            None => added.push(file_path.clone()),
            Some(t) if t.content_hash != content_hash => modified.push(file_path.clone()),
            _ => {}
        }
    }

    Ok(FileDiffResult {
        files_checked: current_files.len(),
        added,
        modified,
        removed,
    })
}

// =============================================================================
// Framework detection context
// =============================================================================

/// Filesystem-backed `ResolutionContext` sufficient for framework detection.
/// Graph-query methods return empty because the DB hasn't been populated yet,
/// but `detect()` only uses `read_file`, `file_exists`, `get_all_files` and
/// `list_directories`, so that's fine.
struct DetectionContext {
    root_dir: PathBuf,
    root_str: String,
    files: Vec<String>,
}

impl ResolutionContext for DetectionContext {
    fn get_nodes_in_file(&self, _file_path: &str) -> Vec<Node> {
        Vec::new()
    }
    fn get_nodes_by_name(&self, _name: &str) -> Vec<Node> {
        Vec::new()
    }
    fn get_nodes_by_qualified_name(&self, _qualified_name: &str) -> Vec<Node> {
        Vec::new()
    }
    fn get_nodes_by_kind(&self, _kind: NodeKind) -> Vec<Node> {
        Vec::new()
    }
    fn get_nodes_by_lower_name(&self, _lower_name: &str) -> Vec<Node> {
        Vec::new()
    }
    fn get_import_mappings(&self, _file_path: &str, _language: Language) -> Vec<ImportMapping> {
        Vec::new()
    }
    fn get_all_files(&self) -> Vec<String> {
        self.files.clone()
    }
    fn get_project_root(&self) -> &str {
        &self.root_str
    }
    fn file_exists(&self, relative_path: &str) -> bool {
        match validate_path_within_root(&self.root_dir, relative_path) {
            Some(full) => full.exists(),
            None => false,
        }
    }
    fn read_file(&self, relative_path: &str) -> Option<String> {
        let full = validate_path_within_root(&self.root_dir, relative_path)?;
        fs::read(&full)
            .ok()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }
    // Monorepo support — needed by framework detect()s that probe
    // subpackage manifests (e.g. fabric-view looking at
    // packages/<sub>/package.json when the root manifest is just a
    // workspace declaration). Matches the resolver-context shape.
    fn list_directories(&self, relative_path: &str) -> Vec<String> {
        let target = if relative_path == "." || relative_path.is_empty() {
            self.root_dir.clone()
        } else {
            self.root_dir.join(relative_path)
        };
        match fs::read_dir(&target) {
            Ok(entries) => entries
                .flatten()
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect(),
            Err(_) => Vec::new(),
        }
    }
}

// =============================================================================
// extractFromSource dispatcher (bottom of src/extraction/tree-sitter.ts)
// =============================================================================

fn unresolved_ref_to_reference(r: crate::resolution::types::UnresolvedRef) -> UnresolvedReference {
    UnresolvedReference {
        from_node_id: r.from_node_id,
        reference_name: r.reference_name,
        reference_kind: r.reference_kind,
        line: r.line,
        column: r.column,
        file_path: Some(r.file_path),
        language: Some(r.language),
        candidates: r.candidates,
    }
}

/// Extract nodes and edges from source code.
///
/// If `framework_names` is provided, framework-specific extractors matching
/// those names and the file's language are run after the tree-sitter pass.
/// Their nodes/references/errors are merged into the returned result.
pub fn extract_from_source(
    file_path: &str,
    source: &str,
    language: Option<Language>,
    framework_names: Option<&[String]>,
) -> ExtractionResult {
    let detected_language = language.unwrap_or_else(|| detect_language(file_path, Some(source)));
    let file_extension = Path::new(file_path)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default();

    // IDA/Hex-Rays decompiler output is C-like but often not valid C
    // (`.name` thunk symbols, IDA typedefs, huge one-function dumps).
    let mut result = if (detected_language == Language::C || detected_language == Language::Cpp)
        && is_ida_generated_c(file_path, source)
    {
        IdaCExtractor::new(file_path, source, detected_language).extract()
    } else if detected_language == Language::Svelte {
        // Use custom extractor for Svelte
        SvelteExtractor::new(file_path, source, languages::extractor_for).extract()
    } else if detected_language == Language::Vue {
        // Use custom extractor for Vue
        VueExtractor::new(file_path, source, languages::extractor_for).extract()
    } else if detected_language == Language::Liquid {
        // Use custom extractor for Liquid
        LiquidExtractor::new(file_path, source).extract()
    } else if detected_language == Language::Xml {
        // Custom extractor for MyBatis mapper XML. Non-mapper XML returns just a
        // file node so the watcher tracks it without emitting symbols.
        MyBatisExtractor::new(file_path, source).extract()
    } else if is_file_level_only_language(detected_language) {
        // No symbol extraction at this stage — files are tracked at the file-record
        // level only. Framework extractors (Drupal routing yml, Spring `@Value`
        // resolution against application.yml/application.properties) run later and
        // add per-file nodes/references when they apply.
        ExtractionResult::default()
    } else if detected_language == Language::Pascal
        && (file_extension == ".dfm" || file_extension == ".fmx")
    {
        // Use custom extractor for DFM/FMX form files
        DfmExtractor::new(file_path, source).extract()
    } else {
        TreeSitterExtractor::new(
            file_path,
            source,
            Some(detected_language),
            languages::extractor_for(detected_language),
        )
        .extract()
    };

    // Framework-specific extraction (routes, middleware, etc.)
    if let Some(names) = framework_names {
        if !names.is_empty() {
            let matching: Vec<_> = get_all_framework_resolvers()
                .into_iter()
                .filter(|r| names.iter().any(|n| n == r.name()))
                .collect();
            let applicable = get_applicable_frameworks(&matching, detected_language);
            for fw in applicable {
                // TS wraps fw.extract in try/catch pushing a
                // `Framework extractor '{name}' failed: {err}` warning; the Rust
                // extract hooks are infallible, so the catch arm is unreachable
                // and was dropped (documented in notes).
                if let Some(fw_result) = fw.extract(file_path, source) {
                    result.nodes.extend(fw_result.nodes);
                    result.unresolved_references.extend(
                        fw_result
                            .references
                            .into_iter()
                            .map(unresolved_ref_to_reference),
                    );
                }
            }
        }
    }

    result
}

// =============================================================================
// ExtractionOrchestrator
// =============================================================================

/// Outcome of the parallel read+parse stage for one file.
enum BatchOutcome {
    ReadError(String),
    SizeExceeded {
        size: u64,
        max: u64,
    },
    Parsed {
        content: String,
        stats: FileStats,
        result: ExtractionResult,
    },
}

struct BatchItem {
    file_path: String,
    outcome: BatchOutcome,
}

/// Read + size-check + parse a single file. Runs on rayon worker threads, so
/// it must not touch the orchestrator (the DB handle is not `Sync`).
fn read_and_parse(root_dir: &Path, file_path: &str, framework_names: &[String]) -> BatchItem {
    let Some(full_path) = validate_path_within_root(root_dir, file_path) else {
        log_warn(
            "Path traversal blocked in batch reader",
            Some(&serde_json::json!({ "filePath": file_path })),
        );
        return BatchItem {
            file_path: file_path.to_string(),
            outcome: BatchOutcome::ReadError("Path traversal blocked".to_string()),
        };
    };

    let read = fs::read(&full_path).and_then(|bytes| {
        let meta = fs::metadata(&full_path)?;
        Ok((bytes, meta))
    });
    let (content, stats) = match read {
        Ok((bytes, meta)) => (
            String::from_utf8_lossy(&bytes).into_owned(),
            FileStats::from_metadata(&meta),
        ),
        Err(err) => {
            return BatchItem {
                file_path: file_path.to_string(),
                outcome: BatchOutcome::ReadError(err.to_string()),
            };
        }
    };

    // Honour MAX_FILE_SIZE. IDA dumps get a higher cap because they are
    // commonly one generated function per file and use a lightweight
    // extractor that does not feed the full text to tree-sitter.
    let max_file_size = if is_ida_generated_c(file_path, &content) {
        MAX_IDA_FILE_SIZE
    } else {
        MAX_FILE_SIZE
    };
    if stats.size > max_file_size {
        return BatchItem {
            file_path: file_path.to_string(),
            outcome: BatchOutcome::SizeExceeded {
                size: stats.size,
                max: max_file_size,
            },
        };
    }

    let language = detect_language(file_path, Some(&content));
    let result = extract_from_source(file_path, &content, Some(language), Some(framework_names));
    BatchItem {
        file_path: file_path.to_string(),
        outcome: BatchOutcome::Parsed {
            content,
            stats,
            result,
        },
    }
}

fn aborted_error() -> ExtractionError {
    ExtractionError {
        message: "Aborted".to_string(),
        file_path: None,
        line: None,
        column: None,
        severity: Severity::Error,
        code: None,
    }
}

fn extraction_error_result(message: String, file_path: &str, code: &str) -> ExtractionResult {
    ExtractionResult {
        errors: vec![ExtractionError {
            message,
            file_path: Some(file_path.to_string()),
            line: None,
            column: None,
            severity: Severity::Error,
            code: Some(code.to_string()),
        }],
        ..Default::default()
    }
}

fn emit(on_progress: Option<&dyn Fn(&IndexProgress)>, progress: IndexProgress) {
    if let Some(cb) = on_progress {
        cb(&progress);
    }
}

fn is_aborted(signal: Option<&AtomicBool>) -> bool {
    signal.is_some_and(|s| s.load(Ordering::Relaxed))
}

/// Extraction orchestrator.
pub struct ExtractionOrchestrator<'a> {
    root_dir: PathBuf,
    queries: &'a QueryBuilder,
    /// Names of frameworks detected for this project, populated by `index_all()`.
    /// Passed to `extract_from_source` so framework-specific extractors (route
    /// nodes, middleware, etc.) run after the tree-sitter pass. Cleared if
    /// detection hasn't run yet so single-file re-index paths can detect on
    /// the spot. (`RefCell`: the TS class mutates this through `&self`-shaped
    /// call paths.)
    detected_framework_names: RefCell<Option<Vec<String>>>,
}

impl<'a> ExtractionOrchestrator<'a> {
    pub fn new(root_dir: impl Into<PathBuf>, queries: &'a QueryBuilder) -> Self {
        ExtractionOrchestrator {
            root_dir: root_dir.into(),
            queries,
            detected_framework_names: RefCell::new(None),
        }
    }

    /// Detect frameworks on demand using the current scanned files (or a fresh
    /// scan if none are provided). Cached on the orchestrator so repeat calls
    /// inside a single run don't re-scan.
    fn ensure_detected_frameworks(&self, files: Option<&[String]>) -> Vec<String> {
        if let Some(names) = self.detected_framework_names.borrow().as_ref() {
            return names.clone();
        }
        let file_list: Vec<String> = match files {
            Some(f) => f.to_vec(),
            None => scan_directory(&self.root_dir, None),
        };
        let context = DetectionContext {
            root_str: self.root_dir.to_string_lossy().into_owned(),
            root_dir: self.root_dir.clone(),
            files: file_list,
        };
        let names: Vec<String> = detect_frameworks(&context)
            .iter()
            .map(|r| r.name().to_string())
            .collect();
        *self.detected_framework_names.borrow_mut() = Some(names.clone());
        names
    }

    pub fn reset_detected_frameworks(&self) {
        *self.detected_framework_names.borrow_mut() = None;
    }

    pub fn reconcile_removed_files(&self) -> Result<ReconcileResult> {
        let current_files: HashSet<String> =
            scan_directory(&self.root_dir, None).into_iter().collect();
        // TS uses a Set — preserve first-insertion order for the output array.
        let mut removed_node_names: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut files_removed = 0usize;

        for tracked in self.queries.get_all_files()? {
            if current_files.contains(&tracked.path) && self.root_dir.join(&tracked.path).exists() {
                continue;
            }
            for node in self.queries.get_nodes_by_file(&tracked.path)? {
                if seen.insert(node.name.clone()) {
                    removed_node_names.push(node.name);
                }
            }
            self.queries.delete_file(&tracked.path)?;
            files_removed += 1;
        }

        Ok(ReconcileResult {
            files_removed,
            removed_node_names,
        })
    }

    /// Index all files in the project.
    ///
    /// `signal`: cooperative abort flag (TS `AbortSignal`) — set to `true` to
    /// abort. `verbose` is kept for signature parity; the TS verbose logs were
    /// all worker-lifecycle messages that have no native equivalent.
    pub fn index_all(
        &self,
        on_progress: Option<&dyn Fn(&IndexProgress)>,
        signal: Option<&AtomicBool>,
        _verbose: bool,
    ) -> Result<IndexResult> {
        init_grammars();
        let start_time = now_ms();
        let mut errors: Vec<ExtractionError> = Vec::new();
        let mut files_indexed = 0usize;
        let mut files_skipped = 0usize;
        let mut files_errored = 0usize;
        let mut total_nodes = 0usize;
        let mut total_edges = 0usize;

        // Phase 1: Scan for files
        emit(
            on_progress,
            IndexProgress {
                phase: IndexPhase::Scanning,
                current: 0,
                total: 0,
                current_file: None,
            },
        );

        let files = match on_progress {
            Some(cb) => {
                let mut scan_cb = |current: usize, file: &str| {
                    cb(&IndexProgress {
                        phase: IndexPhase::Scanning,
                        current,
                        total: 0,
                        current_file: Some(file.to_string()),
                    });
                };
                scan_directory(&self.root_dir, Some(&mut scan_cb))
            }
            None => scan_directory(&self.root_dir, None),
        };

        // Detect frameworks once per index_all run using the scanned file list.
        // Names are passed to each parse call so framework-specific extractors
        // (route nodes, middleware, etc.) run after the tree-sitter pass.
        // Framework detection is reset each run so adding e.g. requirements.txt
        // between runs is picked up without restarting the process.
        *self.detected_framework_names.borrow_mut() = None;
        let framework_names = self.ensure_detected_frameworks(Some(&files));

        if is_aborted(signal) {
            return Ok(IndexResult {
                success: false,
                files_indexed: 0,
                files_skipped: 0,
                files_errored: 0,
                nodes_created: 0,
                edges_created: 0,
                errors: vec![aborted_error()],
                duration_ms: now_ms() - start_time,
            });
        }

        // Phase 2: Parse files (rayon over read batches; storage stays on this
        // thread — SQLite access is single-threaded).
        let total = files.len();
        let mut processed = 0usize;

        emit(
            on_progress,
            IndexProgress {
                phase: IndexPhase::Parsing,
                current: 0,
                total,
                current_file: None,
            },
        );

        // Detect needed languages and load grammars (no-op shims natively;
        // kept for call parity with the TS pipeline).
        let mut needed_languages: Vec<Language> = Vec::new();
        for f in &files {
            let lang = detect_language(f, None);
            if !needed_languages.contains(&lang) {
                needed_languages.push(lang);
            }
        }
        // .h files default to 'c' but may be C++ — ensure cpp grammar is loaded when c is needed
        if needed_languages.contains(&Language::C) && !needed_languages.contains(&Language::Cpp) {
            needed_languages.push(Language::Cpp);
        }
        load_grammars_for_languages(&needed_languages);

        for batch in files.chunks(FILE_IO_BATCH_SIZE) {
            if is_aborted(signal) {
                let mut all_errors = vec![aborted_error()];
                all_errors.extend(errors);
                return Ok(IndexResult {
                    success: false,
                    files_indexed,
                    files_skipped,
                    files_errored,
                    nodes_created: total_nodes,
                    edges_created: total_edges,
                    errors: all_errors,
                    duration_ms: now_ms() - start_time,
                });
            }

            // Read + parse the batch in parallel (with path validation before
            // any I/O); order is preserved.
            let root_dir = self.root_dir.clone();
            let batch_items: Vec<BatchItem> = batch
                .par_iter()
                .map(|fp| read_and_parse(&root_dir, fp, &framework_names))
                .collect();

            // Store results on this thread (SQLite is not thread-safe).
            for item in batch_items {
                if is_aborted(signal) {
                    let mut all_errors = vec![aborted_error()];
                    all_errors.extend(errors);
                    return Ok(IndexResult {
                        success: false,
                        files_indexed,
                        files_skipped,
                        files_errored,
                        nodes_created: total_nodes,
                        edges_created: total_edges,
                        errors: all_errors,
                        duration_ms: now_ms() - start_time,
                    });
                }

                // Report progress before handling (show current file being worked on)
                emit(
                    on_progress,
                    IndexProgress {
                        phase: IndexPhase::Parsing,
                        current: processed,
                        total,
                        current_file: Some(item.file_path.clone()),
                    },
                );

                match item.outcome {
                    BatchOutcome::ReadError(message) => {
                        processed += 1;
                        files_errored += 1;
                        errors.push(ExtractionError {
                            message: format!("Failed to read file: {message}"),
                            file_path: Some(item.file_path),
                            line: None,
                            column: None,
                            severity: Severity::Error,
                            code: Some("read_error".to_string()),
                        });
                    }
                    BatchOutcome::SizeExceeded { size, max } => {
                        processed += 1;
                        files_skipped += 1;
                        errors.push(ExtractionError {
                            message: format!("File exceeds max size ({size} > {max})"),
                            file_path: Some(item.file_path),
                            line: None,
                            column: None,
                            severity: Severity::Warning,
                            code: Some("size_exceeded".to_string()),
                        });
                        emit(
                            on_progress,
                            IndexProgress {
                                phase: IndexPhase::Parsing,
                                current: processed,
                                total,
                                current_file: None,
                            },
                        );
                    }
                    BatchOutcome::Parsed {
                        content,
                        stats,
                        mut result,
                    } => {
                        processed += 1;

                        // Store in database (errors stored on the file record are
                        // pre-filePath-fill, matching the TS serialization order).
                        if !result.nodes.is_empty() || result.errors.is_empty() {
                            let language = detect_language(&item.file_path, Some(&content));
                            self.store_extraction_result(
                                &item.file_path,
                                &content,
                                language,
                                &stats,
                                &result,
                            )?;
                        }

                        if !result.errors.is_empty() {
                            for err in result.errors.iter_mut() {
                                if err.file_path.is_none() {
                                    err.file_path = Some(item.file_path.clone());
                                }
                            }
                            errors.extend(result.errors.iter().cloned());
                        }

                        if !result.nodes.is_empty() {
                            files_indexed += 1;
                            total_nodes += result.nodes.len();
                            total_edges += result.edges.len();
                        } else if result.errors.iter().any(|e| e.severity == Severity::Error) {
                            files_errored += 1;
                        } else {
                            // Files with no symbols but no errors (yaml, twig, properties) are
                            // tracked at the file level — count them as indexed so the CLI
                            // doesn't misleadingly report "No files found to index".
                            let lang = detect_language(&item.file_path, Some(&content));
                            if is_file_level_only_language(lang) {
                                files_indexed += 1;
                            } else {
                                files_skipped += 1;
                            }
                        }
                    }
                }
            }
        }

        // Report 100% so the progress bar doesn't hang at 99%
        emit(
            on_progress,
            IndexProgress {
                phase: IndexPhase::Parsing,
                current: total,
                total,
                current_file: None,
            },
        );

        // (The TS WASM memory-error retry pass is N/A natively and was dropped.)

        Ok(IndexResult {
            success: files_indexed > 0 || !errors.iter().any(|e| e.severity == Severity::Error),
            files_indexed,
            files_skipped,
            files_errored,
            nodes_created: total_nodes,
            edges_created: total_edges,
            errors,
            duration_ms: now_ms() - start_time,
        })
    }

    /// Index specific files.
    pub fn index_files(&self, file_paths: &[String]) -> Result<IndexResult> {
        let start_time = now_ms();
        let mut errors: Vec<ExtractionError> = Vec::new();
        let mut files_indexed = 0usize;
        let mut files_skipped = 0usize;
        let mut files_errored = 0usize;
        let mut total_nodes = 0usize;
        let mut total_edges = 0usize;

        for file_path in file_paths {
            let result = self.index_file(file_path)?;

            if !result.errors.is_empty() {
                errors.extend(result.errors.iter().cloned());
            }

            if !result.nodes.is_empty() {
                files_indexed += 1;
                total_nodes += result.nodes.len();
                total_edges += result.edges.len();
            } else if result.errors.iter().any(|e| e.severity == Severity::Error) {
                files_errored += 1;
            } else {
                let tracked = self.queries.get_file_by_path(file_path)?;
                match tracked {
                    Some(t) if is_file_level_only_language(t.language) => files_indexed += 1,
                    _ => files_skipped += 1,
                }
            }
        }

        Ok(IndexResult {
            success: files_indexed > 0 || !errors.iter().any(|e| e.severity == Severity::Error),
            files_indexed,
            files_skipped,
            files_errored,
            nodes_created: total_nodes,
            edges_created: total_edges,
            errors,
            duration_ms: now_ms() - start_time,
        })
    }

    /// Index a single file.
    pub fn index_file(&self, relative_path: &str) -> Result<ExtractionResult> {
        let Some(full_path) = validate_path_within_root(&self.root_dir, relative_path) else {
            return Ok(extraction_error_result(
                format!("Path traversal blocked: {relative_path}"),
                relative_path,
                "path_traversal",
            ));
        };

        // Read file content and stats
        let read = fs::metadata(&full_path).and_then(|meta| {
            let bytes = fs::read(&full_path)?;
            Ok((meta, bytes))
        });
        let (stats, content) = match read {
            Ok((meta, bytes)) => (
                FileStats::from_metadata(&meta),
                String::from_utf8_lossy(&bytes).into_owned(),
            ),
            Err(error) => {
                return Ok(extraction_error_result(
                    format!("Failed to read file: {error}"),
                    relative_path,
                    "read_error",
                ));
            }
        };

        self.index_file_with_content(relative_path, &content, &stats)
    }

    /// Index a single file with pre-read content and stats.
    /// Used by the parallel batch reader to avoid redundant file I/O.
    pub fn index_file_with_content(
        &self,
        relative_path: &str,
        content: &str,
        stats: &FileStats,
    ) -> Result<ExtractionResult> {
        // Prevent path traversal
        if validate_path_within_root(&self.root_dir, relative_path).is_none() {
            log_warn(
                "Path traversal blocked in indexFileWithContent",
                Some(&serde_json::json!({ "relativePath": relative_path })),
            );
            return Ok(extraction_error_result(
                "Path traversal blocked".to_string(),
                relative_path,
                "path_traversal",
            ));
        }

        // Check file size. IDA dumps get a higher cap because the custom extractor
        // handles large one-function files without tree-sitter.
        let max_file_size = if is_ida_generated_c(relative_path, content) {
            MAX_IDA_FILE_SIZE
        } else {
            MAX_FILE_SIZE
        };
        if stats.size > max_file_size {
            return Ok(ExtractionResult {
                errors: vec![ExtractionError {
                    message: format!("File exceeds max size ({} > {})", stats.size, max_file_size),
                    file_path: Some(relative_path.to_string()),
                    line: None,
                    column: None,
                    severity: Severity::Warning,
                    code: Some("size_exceeded".to_string()),
                }],
                ..Default::default()
            });
        }

        // Detect language
        let language = detect_language(relative_path, Some(content));
        if !is_language_supported(language) {
            return Ok(ExtractionResult::default());
        }

        // Extract from source. Use cached framework names if index_all has run,
        // otherwise detect on the spot so single-file re-index paths still emit
        // route nodes / middleware / etc.
        let framework_names = self.ensure_detected_frameworks(None);
        let result = extract_from_source(
            relative_path,
            content,
            Some(language),
            Some(&framework_names),
        );

        // Store in database
        if !result.nodes.is_empty() || result.errors.is_empty() {
            self.store_extraction_result(relative_path, content, language, stats, &result)?;
        }

        Ok(result)
    }

    /// Store extraction result in database.
    fn store_extraction_result(
        &self,
        file_path: &str,
        content: &str,
        language: Language,
        stats: &FileStats,
        result: &ExtractionResult,
    ) -> Result<()> {
        let content_hash = hash_content(content);

        // Check if file already exists and hasn't changed
        let existing_file = self.queries.get_file_by_path(file_path)?;
        if let Some(existing) = &existing_file {
            if existing.content_hash == content_hash {
                return Ok(()); // No changes
            }
        }

        // Delete existing data for this file
        if existing_file.is_some() {
            self.queries.delete_file(file_path)?;
        }

        // Filter out nodes with missing required fields before insertion.
        // This prevents FK violations when edges reference nodes that would
        // be silently skipped by insert_node() (see issue #42).
        let valid_nodes: Vec<Node> = result
            .nodes
            .iter()
            .filter(|n| !n.id.is_empty() && !n.name.is_empty() && !n.file_path.is_empty())
            .cloned()
            .collect();

        // Insert nodes
        if !valid_nodes.is_empty() {
            self.queries.insert_nodes(&valid_nodes)?;
        }

        let inserted_ids: HashSet<&str> = valid_nodes.iter().map(|n| n.id.as_str()).collect();

        // Filter edges to only reference nodes that were actually inserted
        if !result.edges.is_empty() {
            let valid_edges: Vec<crate::types::Edge> = result
                .edges
                .iter()
                .filter(|e| {
                    inserted_ids.contains(e.source.as_str())
                        && inserted_ids.contains(e.target.as_str())
                })
                .cloned()
                .collect();
            if !valid_edges.is_empty() {
                self.queries.insert_edges(&valid_edges)?;
            }
        }

        // Insert unresolved references in batch with denormalized filePath/language
        if !result.unresolved_references.is_empty() {
            let refs_with_context: Vec<UnresolvedReference> = result
                .unresolved_references
                .iter()
                .filter(|r| inserted_ids.contains(r.from_node_id.as_str()))
                .map(|r| {
                    let mut r = r.clone();
                    if r.file_path.is_none() {
                        r.file_path = Some(file_path.to_string());
                    }
                    if r.language.is_none() {
                        r.language = Some(language);
                    }
                    r
                })
                .collect();
            if !refs_with_context.is_empty() {
                self.queries
                    .insert_unresolved_refs_batch(&refs_with_context)?;
            }
        }

        // Insert file record
        let file_record = FileRecord {
            path: file_path.to_string(),
            content_hash,
            language,
            size: stats.size,
            modified_at: stats.modified_at_ms,
            indexed_at: now_ms(),
            node_count: result.nodes.len() as u32,
            errors: if result.errors.is_empty() {
                None
            } else {
                Some(result.errors.clone())
            },
        };
        self.queries.upsert_file(&file_record)
    }

    /// Sync the index with the current file state.
    ///
    /// Change detection is filesystem-based, never git: a (size, mtime) stat
    /// pre-filter skips unchanged files, then a content-hash compare confirms real
    /// changes. This works in non-git projects and catches committed changes from
    /// `git pull`/`checkout`/`merge`/`rebase` that `git status` cannot see.
    pub fn sync(&self, on_progress: Option<&dyn Fn(&IndexProgress)>) -> Result<SyncResult> {
        init_grammars();
        let start_time = now_ms();
        let mut nodes_updated = 0usize;
        let mut changed_file_paths: Vec<String> = Vec::new();
        // TS uses a Set — preserve first-insertion order.
        let mut changed_node_names: Vec<String> = Vec::new();
        let mut changed_seen: HashSet<String> = HashSet::new();

        emit(
            on_progress,
            IndexProgress {
                phase: IndexPhase::Scanning,
                current: 0,
                total: 0,
                current_file: None,
            },
        );

        let diff = diff_filesystem_against_index(&self.root_dir, self.queries)?;
        let files_checked = diff.files_checked;
        let files_added = diff.added.len();
        let files_modified = diff.modified.len();
        let files_removed = diff.removed.len();
        let files_to_index: Vec<String> = diff
            .added
            .iter()
            .chain(diff.modified.iter())
            .cloned()
            .collect();
        changed_file_paths.extend(files_to_index.iter().cloned());

        for file_path in &diff.removed {
            for node in self.queries.get_nodes_by_file(file_path)? {
                if changed_seen.insert(node.name.clone()) {
                    changed_node_names.push(node.name);
                }
            }
            self.queries.delete_file(file_path)?;
        }

        // Load only grammars needed for changed files (no-op shim natively)
        if !files_to_index.is_empty() {
            let mut needed_languages: Vec<Language> = Vec::new();
            for f in &files_to_index {
                let lang = detect_language(f, None);
                if !needed_languages.contains(&lang) {
                    needed_languages.push(lang);
                }
            }
            // .h files default to 'c' but may be C++ — ensure cpp grammar is loaded
            if needed_languages.contains(&Language::C) && !needed_languages.contains(&Language::Cpp)
            {
                needed_languages.push(Language::Cpp);
            }
            load_grammars_for_languages(&needed_languages);
        }

        // Index changed files
        let total = files_to_index.len();
        for (i, file_path) in files_to_index.iter().enumerate() {
            emit(
                on_progress,
                IndexProgress {
                    phase: IndexPhase::Parsing,
                    current: i + 1,
                    total,
                    current_file: Some(file_path.clone()),
                },
            );

            let before_file = self.queries.get_file_by_path(file_path)?;
            for node in self.queries.get_nodes_by_file(file_path)? {
                if changed_seen.insert(node.name.clone()) {
                    changed_node_names.push(node.name);
                }
            }

            let result = self.index_file(file_path)?;
            nodes_updated += result.nodes.len();
            for node in &result.nodes {
                if changed_seen.insert(node.name.clone()) {
                    changed_node_names.push(node.name.clone());
                }
            }

            // If a previously-indexed file is now unreadable, too large, or otherwise
            // unindexable, remove its old graph state. Missing beats stale: callers can
            // fall back to direct file reads, but stale symbols make tool answers wrong.
            let after_file = self.queries.get_file_by_path(file_path)?;
            if let (Some(before), Some(after)) = (&before_file, &after_file) {
                if after.content_hash == before.content_hash {
                    self.queries.delete_file(file_path)?;
                }
            }
        }

        Ok(SyncResult {
            files_checked,
            files_added,
            files_modified,
            files_removed,
            nodes_updated,
            duration_ms: now_ms() - start_time,
            changed_file_paths: if changed_file_paths.is_empty() {
                None
            } else {
                Some(changed_file_paths)
            },
            changed_node_names: if changed_node_names.is_empty() {
                None
            } else {
                Some(changed_node_names)
            },
        })
    }

    /// Get files that have changed since last index.
    /// Uses filesystem-vs-DB state rather than git status so clean-tree changes
    /// from pull/checkout/merge are still reported as stale.
    pub fn get_changed_files(&self) -> Result<ChangedFiles> {
        let diff = diff_filesystem_against_index(&self.root_dir, self.queries)?;
        Ok(ChangedFiles {
            added: diff.added,
            modified: diff.modified,
            removed: diff.removed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_content_is_sha256_hex() {
        // sha256("") and sha256("hello") — well-known vectors, identical to
        // Node's crypto.createHash('sha256').update(s).digest('hex').
        assert_eq!(
            hash_content(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hash_content("hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn default_ignore_patterns_include_dirs_and_globs() {
        let patterns = default_ignore_patterns();
        assert!(patterns.contains(&"node_modules/".to_string()));
        assert!(patterns.contains(&"__pycache__/".to_string()));
        assert!(patterns.contains(&"*.egg-info/".to_string()));
        assert!(patterns.contains(&"cmake-build-*/".to_string()));
        assert!(patterns.contains(&"bazel-*/".to_string()));
        // first-party-prone names must NOT be listed
        assert!(!patterns.contains(&"src/".to_string()));
        assert!(!patterns.contains(&"lib/".to_string()));
    }

    #[test]
    fn build_default_ignore_excludes_defaults_and_honors_negation() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "!vendor/\nsecret.ts\n").unwrap();
        let ig = build_default_ignore(dir.path());

        // default dir ignored at any depth
        assert!(gitignore_ignores(&ig, "node_modules/pkg/index.js", false));
        assert!(gitignore_ignores(&ig, "a/b/node_modules/x.ts", false));
        // .gitignore negation re-includes a default
        assert!(!gitignore_ignores(&ig, "vendor/lib.go", false));
        // .gitignore additions apply
        assert!(gitignore_ignores(&ig, "secret.ts", false));
        // normal source kept
        assert!(!gitignore_ignores(&ig, "src/index.ts", false));
    }

    #[test]
    fn codegraphignore_is_merged() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".codegraphignore"),
            "research/decompiled-references/\n",
        )
        .unwrap();
        let ig = build_default_ignore(dir.path());
        assert!(gitignore_ignores(
            &ig,
            "research/decompiled-references/all/generated.c",
            false
        ));
        assert!(!gitignore_ignores(&ig, "src/main.rs", false));
    }

    #[test]
    fn extract_from_source_routes_to_file_level_only() {
        let result = extract_from_source("app.yaml", "name: test\n", None, None);
        assert!(result.nodes.is_empty());
        assert!(result.errors.is_empty());
        assert_eq!(result.duration_ms, 0.0);
    }

    #[test]
    fn framework_registry_matches_ts_order_and_names() {
        // extract_from_source filters this registry by detected names — sanity
        // check the canonical registry surface it depends on.
        let names: Vec<String> = get_all_framework_resolvers()
            .iter()
            .map(|r| r.name().to_string())
            .collect();
        assert_eq!(names.len(), 23);
        assert_eq!(names[0], "laravel");
        assert!(names.contains(&"express".to_string()));
        assert!(names.contains(&"django".to_string()));
        assert!(names.contains(&"fabric-view".to_string()));
    }
}

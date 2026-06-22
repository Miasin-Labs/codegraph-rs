use std::path::Path;

use ignore::gitignore::{Gitignore, GitignoreBuilder};

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
pub(super) fn default_ignore_patterns() -> Vec<String> {
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
pub(super) fn gitignore_ignores(ig: &Gitignore, rel_path: &str, is_dir: bool) -> bool {
    if rel_path.is_empty() {
        return false;
    }
    ig.matched_path_or_any_parents(rel_path, is_dir).is_ignore()
}

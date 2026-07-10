//! Import path resolution.

use super::aliases::resolve_aliased_import;
use super::cpp::resolve_cpp_include_path;
use super::external::is_external_import;
use super::normalize::{join_posix, normalize_segments, posix_dirname, relative_posix};
use crate::resolution::types::ResolutionContext;
use crate::types::Language;
use crate::utils::normalize_path;

/// Extension resolution order by language
/// (TS: `EXTENSION_RESOLUTION` record; unlisted languages → empty list).
pub(super) fn extension_resolution(language: Language) -> &'static [&'static str] {
    match language {
        Language::Typescript => &[
            ".ts",
            ".tsx",
            ".d.ts",
            ".js",
            ".jsx",
            "/index.ts",
            "/index.tsx",
            "/index.js",
        ],
        Language::Arkts => &[
            ".ets",
            ".ts",
            ".d.ts",
            ".js",
            "/Index.ets",
            "/index.ets",
            "/index.ts",
            "/index.js",
        ],
        Language::Javascript => &[".js", ".jsx", ".mjs", ".cjs", "/index.js", "/index.jsx"],
        Language::Tsx => &[
            ".tsx",
            ".ts",
            ".d.ts",
            ".js",
            ".jsx",
            "/index.tsx",
            "/index.ts",
            "/index.js",
        ],
        Language::Jsx => &[".jsx", ".js", "/index.jsx", "/index.js"],
        // SFC consumers import plain TS/JS, sibling components, and barrels
        // (`./lib` → `./lib/index.ts`). Without a list, relative imports from a
        // `.svelte`/`.vue` file resolve to nothing, so barrel callers vanish (#629).
        Language::Svelte => &[
            ".ts",
            ".js",
            ".svelte",
            ".tsx",
            ".jsx",
            "/index.ts",
            "/index.js",
            "/index.svelte",
        ],
        Language::Vue => &[
            ".ts",
            ".js",
            ".vue",
            ".tsx",
            ".jsx",
            "/index.ts",
            "/index.js",
            "/index.vue",
        ],
        Language::Astro => &[
            ".ts",
            ".js",
            ".astro",
            ".tsx",
            ".jsx",
            "/index.ts",
            "/index.js",
            "/index.astro",
        ],
        Language::Python => &[".py", "/__init__.py"],
        Language::Go => &[".go"],
        Language::Rust => &[".rs", "/mod.rs"],
        Language::Java => &[".java"],
        Language::C => &[".h", ".c"],
        Language::Cpp => &[".h", ".hpp", ".hxx", ".cpp", ".cc", ".cxx"],
        Language::Csharp => &[".cs"],
        Language::Php => &[".php"],
        Language::Ruby => &[".rb"],
        Language::Objc => &[".h", ".m", ".mm"],
        _ => &[],
    }
}

/// Resolve an import path to an actual file
pub fn resolve_import_path(
    import_path: &str,
    from_file: &str,
    language: Language,
    context: &dyn ResolutionContext,
) -> Option<String> {
    // Skip external/npm packages — but pass the context so the
    // bare-specifier heuristic can consult the project's tsconfig
    // alias map first (custom prefixes like `@components/*` would
    // otherwise be misclassified as npm).
    if is_external_import(import_path, language, Some(context)) {
        return None;
    }

    let project_root = context.get_project_root().to_string();

    // Handle relative imports
    if import_path.starts_with('.') {
        return resolve_relative_import(import_path, from_file, language, context);
    }

    // Handle absolute/aliased imports (like @/ or src/)
    let aliased = resolve_aliased_import(import_path, &project_root, language, context);
    if aliased.is_some() {
        return aliased;
    }

    // C/C++ include directory search: when neither relative nor aliased
    // resolution found a match, search -I directories from
    // compile_commands.json or heuristic probing.
    if language == Language::C || language == Language::Cpp {
        return resolve_cpp_include_path(import_path, language, context);
    }

    None
}

/// Resolve a relative import
fn resolve_relative_import(
    import_path: &str,
    from_file: &str,
    language: Language,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let project_root = normalize_path(context.get_project_root());
    let extensions = extension_resolution(language);

    // Try the path as-is first
    // (TS: `path.resolve(path.dirname(path.join(projectRoot, fromFile)), importPath)`
    //  then `path.relative(projectRoot, basePath)` — done lexically here.)
    let from_dir = posix_dirname(&join_posix(&project_root, &normalize_path(from_file)));
    let base_path = normalize_segments(&join_posix(&from_dir, &normalize_path(import_path)));
    let relative_path = relative_posix(&normalize_segments(&project_root), &base_path);

    // Try each extension
    for ext in extensions {
        let candidate_path = format!("{relative_path}{ext}");
        if context.file_exists(&candidate_path) {
            return Some(candidate_path);
        }
    }

    // Try without extension (might already have one)
    if context.file_exists(&relative_path) {
        return Some(relative_path);
    }

    None
}

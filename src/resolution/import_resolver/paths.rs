//! Import Resolver
//!
//! Resolves import paths to actual files and symbols.
//!
//! Ported from `src/resolution/import-resolver.ts`.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{LazyLock, Mutex};

use regex::Regex;

use crate::resolution::go_module::go_package_dir_for_import;
use crate::resolution::path_aliases::{apply_aliases, relative_lexical};
use crate::resolution::types::{
    ImportMapping,
    ReExport,
    ResolutionContext,
    ResolvedBy,
    ResolvedRef,
    UnresolvedRef,
};
use crate::resolution::workspace_packages::resolve_workspace_import;
use crate::types::{Language, Node, NodeKind};
use crate::utils::{lexical_resolve, normalize_path};

/// Extension resolution order by language
/// (TS: `EXTENSION_RESOLUTION` record; unlisted languages → empty list).
fn extension_resolution(language: Language) -> &'static [&'static str] {
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

/// C and C++ standard library header names (without delimiters).
/// Used by is_external_import to filter system includes from resolution.
static C_CPP_STDLIB_HEADERS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        // C standard library headers
        "assert.h",
        "complex.h",
        "ctype.h",
        "errno.h",
        "fenv.h",
        "float.h",
        "inttypes.h",
        "iso646.h",
        "limits.h",
        "locale.h",
        "math.h",
        "setjmp.h",
        "signal.h",
        "stdalign.h",
        "stdarg.h",
        "stdatomic.h",
        "stdbool.h",
        "stddef.h",
        "stdint.h",
        "stdio.h",
        "stdlib.h",
        "stdnoreturn.h",
        "string.h",
        "tgmath.h",
        "threads.h",
        "time.h",
        "uchar.h",
        "wchar.h",
        "wctype.h",
        // C++ C-library wrappers (cname form)
        "cassert",
        "ccomplex",
        "cctype",
        "cerrno",
        "cfenv",
        "cfloat",
        "cinttypes",
        "ciso646",
        "climits",
        "clocale",
        "cmath",
        "csetjmp",
        "csignal",
        "cstdalign",
        "cstdarg",
        "cstdbool",
        "cstddef",
        "cstdint",
        "cstdio",
        "cstdlib",
        "cstring",
        "ctgmath",
        "ctime",
        "cuchar",
        "cwchar",
        "cwctype",
        // C++ STL headers
        "algorithm",
        "any",
        "array",
        "atomic",
        "barrier",
        "bit",
        "bitset",
        "charconv",
        "chrono",
        "codecvt",
        "compare",
        "complex",
        "concepts",
        "condition_variable",
        "coroutine",
        "deque",
        "exception",
        "execution",
        "expected",
        "filesystem",
        "format",
        "forward_list",
        "fstream",
        "functional",
        "future",
        "generator",
        "initializer_list",
        "iomanip",
        "ios",
        "iosfwd",
        "iostream",
        "istream",
        "iterator",
        "latch",
        "limits",
        "list",
        "locale",
        "map",
        "mdspan",
        "memory",
        "memory_resource",
        "mutex",
        "new",
        "numbers",
        "numeric",
        "optional",
        "ostream",
        "print",
        "queue",
        "random",
        "ranges",
        "ratio",
        "regex",
        "scoped_allocator",
        "semaphore",
        "set",
        "shared_mutex",
        "source_location",
        "span",
        "spanstream",
        "sstream",
        "stack",
        "stacktrace",
        "stdexcept",
        "stdfloat",
        "stop_token",
        "streambuf",
        "string",
        "string_view",
        "strstream",
        "syncstream",
        "system_error",
        "thread",
        "tuple",
        "type_traits",
        "typeindex",
        "typeinfo",
        "unordered_map",
        "unordered_set",
        "utility",
        "valarray",
        "variant",
        "vector",
        "version",
    ]
    .into_iter()
    .collect()
});

/// Check if an import is external (npm package, etc.)
///
/// `context` is consulted for project-defined path aliases
/// (tsconfig/jsconfig `paths`). Without that check, custom prefixes
/// like `@components/*` would fail the bare-specifier heuristic and
/// be classified as external before alias resolution can run.
fn is_external_import(
    import_path: &str,
    language: Language,
    context: Option<&dyn ResolutionContext>,
) -> bool {
    // Relative imports are not external
    if import_path.starts_with('.') {
        return false;
    }

    // Workspace-member imports (`@scope/ui`, `@scope/ui/widgets`) are LOCAL to
    // a monorepo even though they look like bare npm specifiers. Consult the
    // workspace map first so they aren't misclassified as external (#629). The
    // map is None for single-package repos, so this is a no-op there.
    if let Some(ctx) = context {
        if let Some(workspaces) = ctx.get_workspace_packages() {
            if resolve_workspace_import(import_path, workspaces).is_some() {
                return false;
            }
        }
    }

    // Common external patterns
    if matches!(
        language,
        Language::Typescript | Language::Javascript | Language::Tsx | Language::Jsx
    ) {
        // Node built-ins
        if [
            "fs",
            "path",
            "os",
            "crypto",
            "http",
            "https",
            "url",
            "util",
            "events",
            "stream",
            "child_process",
            "buffer",
        ]
        .contains(&import_path)
        {
            return true;
        }
        // Project-defined alias prefix? Treat as local.
        if let Some(aliases) = context.and_then(|c| c.get_project_aliases()) {
            for pat in &aliases.patterns {
                if import_path.starts_with(&pat.prefix) {
                    return false;
                }
            }
        }
        // Scoped packages or bare specifiers that don't start with aliases
        if !import_path.starts_with("@/")
            && !import_path.starts_with("~/")
            && !import_path.starts_with("src/")
        {
            // Likely an npm package
            return true;
        }
    }

    if language == Language::Python {
        // Standard library modules
        let std_libs = [
            "os",
            "sys",
            "json",
            "re",
            "math",
            "datetime",
            "collections",
            "typing",
            "pathlib",
            "logging",
        ];
        let first = import_path.split('.').next().unwrap_or("");
        if std_libs.contains(&first) {
            return true;
        }
    }

    if language == Language::Go {
        // Relative imports (rare in idiomatic Go but the grammar allows them).
        if import_path.starts_with('.') {
            return false;
        }
        // In-module imports look like `<module-path>/sub/pkg` — local to
        // this project. Without the module-path check we'd flag every
        // cross-package call in a Go monorepo as external (issue #388).
        if let Some(module) = context.and_then(|c| c.get_go_module()) {
            if module.matching_root(import_path).is_some() {
                return false;
            }
        }
        // `internal/` packages stay local even when go.mod is missing —
        // preserves the pre-#388 escape hatch for repos without a parsed module path.
        if import_path.contains("/internal/") {
            return false;
        }
        // Anything else is the Go standard library or a third-party module.
        return true;
    }

    if language == Language::C || language == Language::Cpp {
        // C/C++ standard library headers — both C-style (<stdio.h>) and
        // C++-style (<cstdio>, <vector>) forms. Checked against the import
        // path (which the extractor strips of <> or "" delimiters).
        if C_CPP_STDLIB_HEADERS.contains(import_path) {
            return true;
        }
        // C++ headers without .h extension (e.g. "vector", "string")
        let without_ext = import_path.strip_suffix(".h").unwrap_or(import_path);
        if C_CPP_STDLIB_HEADERS.contains(without_ext) {
            return true;
        }
    }

    false
}

// =============================================================================
// Lexical '/'-separated path helpers (Node `path` parity for the
// project-relative posix paths the resolution context deals in)
// =============================================================================

/// Lexical `path.dirname` for '/'-separated paths.
fn posix_dirname(p: &str) -> String {
    match p.rfind('/') {
        Some(0) => "/".to_string(),
        Some(idx) => p[..idx].to_string(),
        None => ".".to_string(),
    }
}

/// Lexical `path.join(a, b)` for '/'-separated paths (no normalization).
fn join_posix(a: &str, b: &str) -> String {
    if a.is_empty() {
        b.to_string()
    } else if b.is_empty() {
        a.to_string()
    } else {
        format!("{}/{}", a.trim_end_matches('/'), b)
    }
}

/// Normalize `.`/`..` segments lexically. Keeps a leading `/` (absolute)
/// and keeps leading `..` segments for relative paths — an import that
/// escapes the project root yields a `../…` candidate that then fails
/// `fileExists`, matching the TS `path.resolve` + `path.relative` outcome.
fn normalize_segments(p: &str) -> String {
    let absolute = p.starts_with('/');
    let mut stack: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => match stack.last() {
                Some(&"..") | None => {
                    if !absolute {
                        stack.push("..");
                    }
                    // absolute: clamp at the root, like `path.resolve`.
                }
                Some(_) => {
                    stack.pop();
                }
            },
            s => stack.push(s),
        }
    }
    let joined = stack.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

/// Lexical `path.relative(from, to)` for '/'-separated paths that share
/// the same lexical base (both produced from the project root).
fn relative_posix(from: &str, to: &str) -> String {
    if from == to {
        return String::new();
    }
    let from_parts: Vec<&str> = from
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    let to_parts: Vec<&str> = to
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    let mut common = 0;
    while common < from_parts.len()
        && common < to_parts.len()
        && from_parts[common] == to_parts[common]
    {
        common += 1;
    }
    // One ".." per remaining `from` segment (TS: push '..' in a loop).
    let mut parts: Vec<&str> = vec![".."; from_parts.len() - common];
    parts.extend(&to_parts[common..]);
    parts.join("/")
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

/// Resolve an aliased/absolute import.
///
/// Tries, in order:
///   1. Project-defined `compilerOptions.paths` (tsconfig/jsconfig).
///      Each pattern can have multiple replacements; tried in tsconfig
///      priority order with extension permutations.
///   2. The legacy hard-coded fallback list (`@/`, `~/`, `src/`, ...)
///      for projects that have aliases but no tsconfig paths block.
///   3. Direct path lookup (with extensions).
fn resolve_aliased_import(
    import_path: &str,
    project_root: &str,
    language: Language,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let extensions = extension_resolution(language);
    let try_with_ext = |base_path: &str| -> Option<String> {
        for ext in extensions {
            let candidate = format!("{base_path}{ext}");
            if context.file_exists(&candidate) {
                return Some(candidate);
            }
        }
        if context.file_exists(base_path) {
            return Some(base_path.to_string());
        }
        None
    };

    // 1. Project tsconfig/jsconfig paths.
    if let Some(alias_map) = context.get_project_aliases() {
        let candidates = apply_aliases(import_path, alias_map, project_root);
        for c in &candidates {
            if let Some(hit) = try_with_ext(c) {
                return Some(hit);
            }
        }
    }

    // 1.5 Workspace packages (`@scope/ui/widgets` → `packages/ui/widgets`).
    //     Resolves a monorepo member import to the member's directory; the
    //     extension/index permutations below then find its barrel (#629).
    if let Some(workspaces) = context.get_workspace_packages() {
        if let Some(base) = resolve_workspace_import(import_path, workspaces) {
            if let Some(hit) = try_with_ext(&base) {
                return Some(hit);
            }
        }
    }

    // 2. Hard-coded fallback list. Kept for projects that use these
    //    conventional aliases without declaring them in tsconfig.
    let fallback_aliases: [(&str, &str); 6] = [
        ("@/", "src/"),
        ("~/", "src/"),
        ("@src/", "src/"),
        ("src/", "src/"),
        ("@app/", "app/"),
        ("app/", "app/"),
    ];
    for (alias, replacement) in fallback_aliases {
        if let Some(rest) = import_path.strip_prefix(alias) {
            if let Some(hit) = try_with_ext(&format!("{replacement}{rest}")) {
                return Some(hit);
            }
        }
    }

    // 3. Direct path.
    try_with_ext(import_path)
}

/// C/C++ include directory cache (keyed by project root).
/// Loaded once per resolver instance, shared across calls.
static CPP_INCLUDE_DIR_CACHE: LazyLock<Mutex<HashMap<String, Vec<String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Clear the C/C++ include directory cache (call between indexing runs)
pub fn clear_cpp_include_dir_cache() {
    CPP_INCLUDE_DIR_CACHE.lock().unwrap().clear();
}

/// Discover C/C++ include search directories for a project.
///
/// Strategy:
/// 1. Look for compile_commands.json (Clang compilation database) in the
///    project root and common build subdirectories. Parse -I and -isystem
///    flags from compiler commands.
/// 2. If no compilation database is found, probe for common convention
///    directories (include/, src/, lib/, api/) and top-level directories
///    containing .h/.hpp files.
///
/// Returns paths relative to projectRoot.
pub fn load_cpp_include_dirs(project_root: &str) -> Vec<String> {
    if let Some(cached) = CPP_INCLUDE_DIR_CACHE.lock().unwrap().get(project_root) {
        return cached.clone();
    }

    let dirs = load_cpp_include_dirs_from_compile_db(project_root)
        .unwrap_or_else(|| load_cpp_include_dirs_heuristic(project_root));

    CPP_INCLUDE_DIR_CACHE
        .lock()
        .unwrap()
        .insert(project_root.to_string(), dirs.clone());
    dirs
}

/// One entry of a Clang compilation database. Type mismatches fail the
/// whole parse — mirroring the TS version, where a malformed entry threw
/// inside the `try` and the function returned `null`.
#[derive(serde::Deserialize)]
struct CompileDbEntry {
    #[serde(default)]
    directory: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    arguments: Option<Vec<String>>,
}

/// Try to load include directories from compile_commands.json.
/// Returns `None` if no compilation database is found (so the heuristic
/// fallback can run). Returns a vec (possibly empty) otherwise.
fn load_cpp_include_dirs_from_compile_db(project_root: &str) -> Option<Vec<String>> {
    let root = Path::new(project_root);
    let candidates = [
        root.join("compile_commands.json"),
        root.join("build").join("compile_commands.json"),
        root.join("cmake-build-debug").join("compile_commands.json"),
        root.join("cmake-build-release")
            .join("compile_commands.json"),
        root.join("out").join("compile_commands.json"),
    ];

    let db_path = candidates.iter().find(|c| c.exists())?;

    let content = std::fs::read_to_string(db_path).ok()?;
    let entries: Vec<CompileDbEntry> = serde_json::from_str(&content).ok()?;

    // Insertion-ordered set (TS used `Set` + `Array.from`).
    let mut dir_set: Vec<String> = Vec::new();
    for entry in &entries {
        let dir = match entry.directory.as_deref() {
            Some(d) if !d.is_empty() => d,
            _ => project_root,
        };
        let split;
        let args: &[String] = match (&entry.arguments, &entry.command) {
            (Some(args), _) => args,
            (None, Some(cmd)) => {
                split = shlex_split(cmd);
                &split
            }
            (None, None) => &[],
        };
        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            let mut include_dir: Option<&str> = None;
            // -I<dir> (no space)
            if arg.starts_with("-I") && arg.len() > 2 {
                include_dir = Some(&arg[2..]);
            }
            // -isystem <dir> (space-separated)
            else if (arg == "-isystem" || arg == "-I") && i + 1 < args.len() {
                include_dir = Some(&args[i + 1]);
                i += 1; // skip next arg
            }
            if let Some(include_dir) = include_dir {
                // Normalize: resolve relative to the compilation directory
                let abs_path = if Path::new(include_dir).is_absolute() {
                    lexical_resolve(Path::new(""), include_dir)
                } else {
                    lexical_resolve(Path::new(dir), include_dir)
                };
                let rel_path =
                    relative_lexical(&lexical_resolve(Path::new(""), project_root), &abs_path)
                        .replace('\\', "/");
                // Skip system directories and paths outside the project
                // (relative paths starting with .. or absolute paths like
                // /usr/include or C:\usr on Windows)
                if !rel_path.starts_with("..")
                    && !rel_path.is_empty()
                    && !Path::new(&rel_path).is_absolute()
                    && !dir_set.contains(&rel_path)
                {
                    dir_set.push(rel_path);
                }
            }
            i += 1;
        }
    }
    Some(dir_set)
}

/// Minimal shlex-style split for compiler command strings.
/// Handles double-quoted and single-quoted arguments.
fn shlex_split(cmd: &str) -> Vec<String> {
    let chars: Vec<char> = cmd.chars().collect();
    let mut result: Vec<String> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        // Skip whitespace
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let ch = chars[i];
        if ch == '"' {
            i += 1;
            let mut arg = String::new();
            while i < chars.len() && chars[i] != '"' {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 1;
                    arg.push(chars[i]);
                } else {
                    arg.push(chars[i]);
                }
                i += 1;
            }
            i += 1; // closing quote
            result.push(arg);
        } else if ch == '\'' {
            i += 1;
            let mut arg = String::new();
            while i < chars.len() && chars[i] != '\'' {
                arg.push(chars[i]);
                i += 1;
            }
            i += 1; // closing quote
            result.push(arg);
        } else {
            let mut arg = String::new();
            while i < chars.len() && !chars[i].is_whitespace() {
                arg.push(chars[i]);
                i += 1;
            }
            result.push(arg);
        }
    }
    result
}

/// Heuristic include directory discovery when no compile_commands.json exists.
/// Checks common convention directories and scans top-level dirs for headers.
fn load_cpp_include_dirs_heuristic(project_root: &str) -> Vec<String> {
    static HEADER_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\.(h|hpp|hxx|hh)$").expect("valid regex"));
    let mut dirs: Vec<String> = Vec::new();
    let convention_dirs = ["include", "src", "lib", "api", "inc"];

    let entries = match std::fs::read_dir(project_root) {
        Ok(rd) => rd,
        Err(_) => return dirs,
    };
    for entry in entries.flatten() {
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if !is_dir {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        // Convention directories
        if convention_dirs.contains(&name.to_lowercase().as_str()) {
            dirs.push(name);
            continue;
        }
        // Any top-level directory containing .h or .hpp files
        if let Ok(sub) = std::fs::read_dir(Path::new(project_root).join(&name)) {
            let has_header = sub
                .flatten()
                .any(|f| HEADER_RE.is_match(f.file_name().to_string_lossy().as_ref()));
            if has_header {
                dirs.push(name);
            }
        }
        // ignore permission errors
    }

    dirs
}

/// Resolve a C/C++ include path by searching include directories.
/// Called as a fallback after relative and aliased resolution fail.
fn resolve_cpp_include_path(
    import_path: &str,
    language: Language,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let include_dirs = context.get_cpp_include_dirs();
    let extensions = extension_resolution(language);

    for dir in &include_dirs {
        let normalized_dir = dir.replace('\\', "/");
        for ext in extensions {
            let candidate = format!("{normalized_dir}/{import_path}{ext}");
            if context.file_exists(&candidate) {
                return Some(candidate);
            }
        }
        // Try as-is (already has extension)
        let candidate = format!("{normalized_dir}/{import_path}");
        if context.file_exists(&candidate) {
            return Some(candidate);
        }
    }

    None
}

/// Extract import mappings from a file
pub fn extract_import_mappings(
    _file_path: &str,
    content: &str,
    language: Language,
) -> Vec<ImportMapping> {
    let mut mappings: Vec<ImportMapping> = Vec::new();

    match language {
        Language::Typescript | Language::Javascript | Language::Tsx | Language::Jsx => {
            mappings.extend(extract_js_imports(content));
        }
        Language::Svelte | Language::Vue => {
            // Svelte/Vue single-file components import via plain ES6 inside their
            // `<script>` block. Without this, a `.svelte`/`.vue` consumer produces
            // zero import mappings, so `resolveViaImport` can't run and a barrel
            // import (`import { Foo } from './lib'`) falls back to name-matching —
            // which silently fails whenever the re-export alias differs from the
            // component's real name, yielding a false 0 callers (#629). The ES6
            // import regex only matches `import … from '…'`, so running it over the
            // whole SFC (markup + styles included) is safe.
            mappings.extend(extract_js_imports(content));
        }
        Language::Python => mappings.extend(extract_python_imports(content)),
        Language::Go => mappings.extend(extract_go_imports(content)),
        Language::Java | Language::Kotlin => mappings.extend(extract_java_imports(content)),
        Language::Php => mappings.extend(extract_php_imports(content)),
        Language::C | Language::Cpp => mappings.extend(extract_cpp_imports(content)),
        _ => {}
    }

    mappings
}

fn mapping(
    local_name: &str,
    exported_name: &str,
    source: &str,
    is_default: bool,
    is_namespace: bool,
) -> ImportMapping {
    ImportMapping {
        local_name: local_name.to_string(),
        exported_name: exported_name.to_string(),
        source: source.to_string(),
        is_default,
        is_namespace,
        resolved_path: None,
    }
}

// NOTE on regex classes: JS regexes without the `u` flag use ASCII-only
// `\w`; the Rust `regex` crate's `\w` is Unicode. `[0-9A-Za-z_]` is used
// below wherever TS wrote `\w` to keep matching byte-for-byte identical.
const W: &str = "[0-9A-Za-z_]";

/// Extract JS/TS import mappings
fn extract_js_imports(content: &str) -> Vec<ImportMapping> {
    static IMPORT_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&format!(
            r#"import\s+(?:({W}+)\s*,?\s*)?(?:\{{([^}}]+)\}})?\s*(?:(\*)\s+as\s+({W}+))?\s*from\s*['"]([^'"]+)['"]"#
        ))
        .expect("valid regex")
    });
    static NAME_ALIAS_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(&format!(r"({W}+)\s+as\s+({W}+)")).expect("valid regex"));
    static REQUIRE_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&format!(
            r#"(?:const|let|var)\s+(?:({W}+)|\{{([^}}]+)\}})\s*=\s*require\(['"]([^'"]+)['"]\)"#
        ))
        .expect("valid regex")
    });
    static DESTRUCTURE_ALIAS_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(&format!(r"({W}+)\s*:\s*({W}+)")).expect("valid regex"));

    let mut mappings: Vec<ImportMapping> = Vec::new();

    // ES6 imports
    for m in IMPORT_RE.captures_iter(content) {
        let default_import = m.get(1).map(|g| g.as_str());
        let named_imports = m.get(2).map(|g| g.as_str());
        let star = m.get(3).map(|g| g.as_str());
        let namespace_alias = m.get(4).map(|g| g.as_str());
        let source = m.get(5).expect("group 5").as_str();

        // Default import
        if let Some(default_import) = default_import {
            mappings.push(mapping(default_import, "default", source, true, false));
        }

        // Named imports
        if let Some(named_imports) = named_imports {
            for name in named_imports.split(',') {
                let name = name.trim();
                if let Some(alias) = NAME_ALIAS_RE.captures(name) {
                    mappings.push(mapping(
                        alias.get(2).expect("group 2").as_str(),
                        alias.get(1).expect("group 1").as_str(),
                        source,
                        false,
                        false,
                    ));
                } else if !name.is_empty() {
                    mappings.push(mapping(name, name, source, false, false));
                }
            }
        }

        // Namespace import
        if let (Some(_star), Some(namespace_alias)) = (star, namespace_alias) {
            mappings.push(mapping(namespace_alias, "*", source, false, true));
        }
    }

    // Require statements
    for m in REQUIRE_RE.captures_iter(content) {
        let default_name = m.get(1).map(|g| g.as_str());
        let destructured = m.get(2).map(|g| g.as_str());
        let source = m.get(3).expect("group 3").as_str();

        if let Some(default_name) = default_name {
            mappings.push(mapping(default_name, "default", source, true, false));
        }

        if let Some(destructured) = destructured {
            for name in destructured.split(',') {
                let name = name.trim();
                if let Some(alias) = DESTRUCTURE_ALIAS_RE.captures(name) {
                    mappings.push(mapping(
                        alias.get(2).expect("group 2").as_str(),
                        alias.get(1).expect("group 1").as_str(),
                        source,
                        false,
                        false,
                    ));
                } else if !name.is_empty() {
                    mappings.push(mapping(name, name, source, false, false));
                }
            }
        }
    }

    mappings
}

/// Extract Python import mappings
fn extract_python_imports(content: &str) -> Vec<ImportMapping> {
    static FROM_IMPORT_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&format!(
            r"from\s+([{w}.]+)\s+import\s+([^#\n]+)",
            w = "0-9A-Za-z_"
        ))
        .expect("valid regex")
    });
    static NAME_ALIAS_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(&format!(r"({W}+)\s+as\s+({W}+)")).expect("valid regex"));
    static IMPORT_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&format!(
            r"(?m)^import\s+([{w}.]+)(?:\s+as\s+({W}+))?",
            w = "0-9A-Za-z_"
        ))
        .expect("valid regex")
    });

    let mut mappings: Vec<ImportMapping> = Vec::new();

    // from X import Y
    for m in FROM_IMPORT_RE.captures_iter(content) {
        let source = m.get(1).expect("group 1").as_str();
        let imports = m.get(2).expect("group 2").as_str();

        for name in imports.split(',') {
            let name = name.trim();
            if let Some(alias) = NAME_ALIAS_RE.captures(name) {
                mappings.push(mapping(
                    alias.get(2).expect("group 2").as_str(),
                    alias.get(1).expect("group 1").as_str(),
                    source,
                    false,
                    false,
                ));
            } else if !name.is_empty() && name != "*" {
                mappings.push(mapping(name, name, source, false, false));
            }
        }
    }

    // import X
    for m in IMPORT_RE.captures_iter(content) {
        let source = m.get(1).expect("group 1").as_str();
        let alias = m.get(2).map(|g| g.as_str());
        let local_name = alias.unwrap_or_else(|| source.split('.').next_back().unwrap_or(""));
        mappings.push(mapping(local_name, "*", source, false, true));
    }

    mappings
}

/// Extract Go import mappings
fn extract_go_imports(content: &str) -> Vec<ImportMapping> {
    static SINGLE_IMPORT_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&format!(r#"import\s+(?:({W}+)\s+)?["']([^"']+)["']"#)).expect("valid regex")
    });
    static BLOCK_IMPORT_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)import\s*\(\s*([^)]+)\s*\)").expect("valid regex"));
    static LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&format!(r#"(?:({W}+)\s+)?["']([^"']+)["']"#)).expect("valid regex")
    });

    let mut mappings: Vec<ImportMapping> = Vec::new();

    // import "path" or import alias "path"
    for m in SINGLE_IMPORT_RE.captures_iter(content) {
        let alias = m.get(1).map(|g| g.as_str());
        let source = m.get(2).expect("group 2").as_str();
        let package_name = source.split('/').next_back().unwrap_or("");
        mappings.push(mapping(
            alias.unwrap_or(package_name),
            "*",
            source,
            false,
            true,
        ));
    }

    // import ( ... ) block
    for m in BLOCK_IMPORT_RE.captures_iter(content) {
        let block = m.get(1).expect("group 1").as_str();
        for line in LINE_RE.captures_iter(block) {
            let alias = line.get(1).map(|g| g.as_str());
            let source = line.get(2).expect("group 2").as_str();
            let package_name = source.split('/').next_back().unwrap_or("");
            mappings.push(mapping(
                alias.unwrap_or(package_name),
                "*",
                source,
                false,
                true,
            ));
        }
    }

    mappings
}

/// Extract Java / Kotlin import mappings.
///
/// Java/Kotlin imports carry the full qualified name of the imported
/// symbol — `import com.example.dao.converter.FooConverter;` — which is
/// exactly the disambiguation signal we need when two packages both
/// declare a `FooConverter`. Pre-#314 the resolver had no Java branch
/// here at all, so this mapping was empty and cross-module name
/// collisions were resolved by file-path proximity (often wrongly).
///
/// `import static com.example.Foo.bar;` is parsed as a local-name `bar`
/// pointing at FQN `com.example.Foo.bar` so static-method call sites
/// (`bar(...)`) can resolve through the same import lookup.
fn extract_java_imports(content: &str) -> Vec<ImportMapping> {
    static BLOCK_COMMENT_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)/\*.*?\*/").expect("valid regex"));
    static LINE_COMMENT_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"//[^\n]*").expect("valid regex"));
    static IMPORT_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&format!(
            r"(?m)^\s*import\s+(static\s+)?([{w}.]+(?:\.\*)?)\s*;",
            w = "0-9A-Za-z_"
        ))
        .expect("valid regex")
    });

    let mut mappings: Vec<ImportMapping> = Vec::new();
    // Strip line and block comments so `// import foo;` doesn't false-match.
    let stripped = BLOCK_COMMENT_RE.replace_all(content, "");
    let stripped = LINE_COMMENT_RE.replace_all(&stripped, "");
    // `import [static] <fqn>[.*];`
    for m in IMPORT_RE.captures_iter(&stripped) {
        let fqn = m.get(2).expect("group 2").as_str();
        // `import com.example.*;` — wildcard. We can't materialize a single
        // local name; skip and let name-matching handle members reachable
        // through the wildcard. (Future enhancement: enumerate package files.)
        if fqn.ends_with(".*") {
            continue;
        }
        let local_name = fqn.split('.').next_back().unwrap_or("");
        if local_name.is_empty() {
            continue;
        }
        mappings.push(mapping(local_name, local_name, fqn, false, false));
    }
    mappings
}

/// Extract PHP import mappings (use statements)
fn extract_php_imports(content: &str) -> Vec<ImportMapping> {
    static USE_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&format!(
            r"use\s+([{w}\\]+)(?:\s+as\s+({W}+))?;",
            w = "0-9A-Za-z_"
        ))
        .expect("valid regex")
    });

    let mut mappings: Vec<ImportMapping> = Vec::new();

    // use Namespace\Class; or use Namespace\Class as Alias;
    for m in USE_RE.captures_iter(content) {
        let full_path = m.get(1).expect("group 1").as_str();
        let alias = m.get(2).map(|g| g.as_str());
        let class_name = full_path.split('\\').next_back().unwrap_or("");
        mappings.push(mapping(
            alias.unwrap_or(class_name),
            class_name,
            full_path,
            false,
            false,
        ));
    }

    mappings
}

/// Extract C/C++ import mappings from #include directives.
///
/// #include brings all symbols from the included header into scope
/// (namespace import), so each mapping uses is_namespace: true and
/// exported_name: '*'. The local_name is set to the header's basename
/// without extension so that symbol references like `MyClass` can
/// match against any include that might provide it.
fn extract_cpp_imports(content: &str) -> Vec<ImportMapping> {
    static INCLUDE_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?m)^\s*#\s*include\s+[<"]([^>"]+)[>"]"#).expect("valid regex")
    });
    static EXT_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\.(h|hpp|hxx|hh|inl|ipp|cxx|cc|cpp)$").expect("valid regex"));

    let mut mappings: Vec<ImportMapping> = Vec::new();

    // Match both #include <...> and #include "..."
    for m in INCLUDE_RE.captures_iter(content) {
        let module_path = m.get(1).expect("group 1").as_str();
        // Basename without extension for localName matching
        let last = module_path.split('/').next_back().unwrap_or("");
        let basename = EXT_RE.replace(last, "").into_owned();
        let local_name = if basename.is_empty() {
            module_path
        } else {
            &basename
        };
        mappings.push(mapping(local_name, "*", module_path, false, true));
    }

    mappings
}

/// Import-mappings-per-file cache. NOTE: vestigial — the TS original
/// declared this cache but never populated it (per-file caching lives in
/// the resolver's `ResolutionContext.getImportMappings`); kept so
/// `clear_import_mapping_cache` mirrors the TS export exactly.
static IMPORT_MAPPING_CACHE: LazyLock<Mutex<HashMap<String, Vec<ImportMapping>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Clear the import mapping cache (call between indexing runs)
pub fn clear_import_mapping_cache() {
    IMPORT_MAPPING_CACHE.lock().unwrap().clear();
    CPP_INCLUDE_DIR_CACHE.lock().unwrap().clear();
}

/// Strip JS line + block comments from `content` while preserving
/// string literals (so `"//"` inside a string stays intact). Used by
/// [`extract_re_exports`] so commented-out export-from statements
/// don't generate phantom re-export edges.
///
/// Scanner is deliberately small: it only tracks the three contexts
/// relevant for JS/TS — single-quote string, double-quote string, and
/// template literal. Comment recognition is the JS spec subset, no
/// regex-literal awareness (which is fine for our use case: we don't
/// apply this to function bodies, only to top-level files).
fn strip_js_comments(content: &str) -> String {
    let bytes = content.as_bytes();
    let mut out = String::with_capacity(content.len());
    let mut i = 0;
    let mut str_delim: Option<u8> = None;
    while i < bytes.len() {
        let ch = bytes[i];
        if let Some(delim) = str_delim {
            if ch == b'\\' && i + 1 < bytes.len() {
                out.push('\\');
                let next_len = utf8_len(bytes[i + 1]);
                out.push_str(&content[i + 1..i + 1 + next_len]);
                i += 1 + next_len;
                continue;
            }
            if ch == delim {
                str_delim = None;
            }
            let ch_len = utf8_len(ch);
            out.push_str(&content[i..i + ch_len]);
            i += ch_len;
            continue;
        }
        if ch == b'"' || ch == b'\'' || ch == b'`' {
            str_delim = Some(ch);
            out.push(ch as char);
            i += 1;
            continue;
        }
        if ch == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if ch == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i += 2;
            while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        let ch_len = utf8_len(ch);
        out.push_str(&content[i..i + ch_len]);
        i += ch_len;
    }
    out
}

/// Length in bytes of the UTF-8 sequence starting with `first_byte`.
fn utf8_len(first_byte: u8) -> usize {
    match first_byte {
        b if b < 0x80 => 1,
        b if b >= 0xF0 => 4,
        b if b >= 0xE0 => 3,
        _ => 2,
    }
}

/// Extract JS/TS re-export declarations from `content`.
///
/// Recognised forms:
///   export { foo } from './a';
///   export { foo as bar } from './a';
///   export * from './a';
///   export * as ns from './a';   (treated as wildcard for chasing)
///   export { default as Foo } from './a';
///
/// The walker intentionally stays regex-based — the import-resolver
/// elsewhere in this file already chooses regex over a fresh
/// tree-sitter pass, and this function shares that trade-off. Errors
/// fall through silently; resolution simply skips the broken file.
pub fn extract_re_exports(content: &str, language: Language) -> Vec<ReExport> {
    if !matches!(
        language,
        Language::Typescript | Language::Javascript | Language::Tsx | Language::Jsx
    ) {
        return Vec::new();
    }

    static WILDCARD_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&format!(
            r#"export\s*\*(?:\s+as\s+{W}+)?\s*from\s*['"]([^'"]+)['"]"#
        ))
        .expect("valid regex")
    });
    static NAMED_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"export\s*\{([^}]+)\}\s*from\s*['"]([^'"]+)['"]"#).expect("valid regex")
    });
    static ALIAS_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(&format!(r"^({W}+)\s+as\s+({W}+)$")).expect("valid regex"));
    static IDENT_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(&format!(r"^{W}+$")).expect("valid regex"));

    let mut out: Vec<ReExport> = Vec::new();

    // Pre-strip block comments + line comments so a commented-out
    // `// export { x } from '...'` doesn't produce a phantom edge.
    // (Template literals are still a possible source of false positives;
    // a project that builds export statements as runtime strings is
    // out of scope.)
    let cleaned = strip_js_comments(content);

    // Wildcard: `export * from '...'` or `export * as ns from '...'`
    for m in WILDCARD_RE.captures_iter(&cleaned) {
        out.push(ReExport::Wildcard {
            source: m.get(1).expect("group 1").as_str().to_string(),
        });
    }

    // Named: `export { a, b as c } from '...'`
    for m in NAMED_RE.captures_iter(&cleaned) {
        let inner = m.get(1).expect("group 1").as_str();
        let source = m.get(2).expect("group 2").as_str();
        for raw in inner.split(',') {
            let item = raw.trim();
            if item.is_empty() {
                continue;
            }
            if let Some(alias) = ALIAS_RE.captures(item) {
                out.push(ReExport::Named {
                    exported_name: alias.get(2).expect("group 2").as_str().to_string(),
                    original_name: alias.get(1).expect("group 1").as_str().to_string(),
                    source: source.to_string(),
                });
            } else if IDENT_RE.is_match(item) {
                out.push(ReExport::Named {
                    exported_name: item.to_string(),
                    original_name: item.to_string(),
                    source: source.to_string(),
                });
            }
        }
    }

    out
}

/// JVM (Java / Kotlin) imports use fully-qualified names (`import
/// com.example.foo.Bar`) decoupled from filenames, so the JS/Python
/// style filesystem path lookup misses them whenever the file isn't
/// named after its primary symbol (Kotlin `Utils.kt` exporting `Bar`,
/// top-level fns, extension fns). Resolve them through the
/// `qualifiedName` index instead — populated by the package_header /
/// package_declaration namespace wrappers in the extractor.
pub fn resolve_jvm_import(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if reference.reference_kind != crate::types::EdgeKind::Imports {
        return None;
    }
    if reference.language != Language::Java && reference.language != Language::Kotlin {
        return None;
    }

    let fqn = &reference.reference_name;
    let last_dot = fqn.rfind('.')?;
    if last_dot == 0 {
        return None;
    }
    let pkg = &fqn[..last_dot];
    let sym = &fqn[last_dot + 1..];
    // Wildcard imports (`com.example.*`) deliberately punt to name-matcher.
    if sym == "*" {
        return None;
    }

    let candidates = context.get_nodes_by_qualified_name(&format!("{pkg}::{sym}"));
    let first = candidates.first()?;

    Some(ResolvedRef {
        original: reference.clone(),
        target_node_id: first.id.clone(),
        confidence: 0.95,
        resolved_by: ResolvedBy::Import,
    })
}

/// Resolve a reference using import mappings
pub fn resolve_via_import(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    // C/C++ #include references — resolve directly to the included file
    // (file→file edge), bypassing symbol lookup. The extractor emits these
    // with `referenceKind: 'imports'` and `referenceName: <include path>`
    // (e.g. "uint256.h" or "common/args.h"). Without this branch the
    // include-dir scan path inside resolveImportPath never produces an
    // edge — resolveViaImport's symbol lookup below would search the
    // resolved file for a symbol named like the file extension and fail.
    if (reference.language == Language::C || reference.language == Language::Cpp)
        && reference.reference_kind == crate::types::EdgeKind::Imports
    {
        let resolved_path = resolve_import_path(
            &reference.reference_name,
            &reference.file_path,
            reference.language,
            context,
        )?;
        let basename = resolved_path.split('/').next_back().unwrap_or("");
        let file_nodes: Vec<Node> = context
            .get_nodes_by_name(basename)
            .into_iter()
            .filter(|n| n.kind == NodeKind::File)
            .collect();
        let file_node = file_nodes.iter().find(|n| n.file_path == resolved_path)?;
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: file_node.id.clone(),
            confidence: 0.9,
            resolved_by: ResolvedBy::Import,
        });
    }

    // Use cached import mappings (avoids re-reading and re-parsing per ref)
    let imports = context.get_import_mappings(&reference.file_path, reference.language);
    if imports.is_empty() {
        // TS: `!context.readFile(ref.filePath)` — falsy covers both a
        // missing file (null) and an empty one ('').
        let content = context.read_file(&reference.file_path);
        if content.is_none_or(|c| c.is_empty()) {
            return None;
        }
    }

    // Go cross-package calls: `pkga.FuncX(...)` extracts to referenceName
    // `pkga.FuncX` and the import `github.com/example/myproject/pkga`
    // maps to a *package directory* containing one or more .go files.
    // The generic file-based lookup below can't follow that — issue #388.
    if reference.language == Language::Go {
        if let Some(go_result) = resolve_go_cross_package_reference(reference, &imports, context) {
            return Some(go_result);
        }
    }

    // Java / Kotlin: imports are FQNs (`import com.example.Foo;`) — no
    // resolvable file path the JS/TS-style chain below could follow. Look
    // up the symbol by name and filter to the candidate whose file path
    // matches the imported FQN. This is the disambiguation signal that
    // breaks the same-name class collision the path-proximity matcher
    // can't resolve (issue #314).
    if reference.language == Language::Java || reference.language == Language::Kotlin {
        if let Some(java_result) = resolve_java_imported_reference(reference, &imports, context) {
            return Some(java_result);
        }
    }

    // Check if the reference name matches any import
    for imp in &imports {
        if imp.local_name == reference.reference_name
            || reference
                .reference_name
                .starts_with(&format!("{}.", imp.local_name))
        {
            // Resolve the import path
            let resolved_path = resolve_import_path(
                &imp.source,
                &reference.file_path,
                reference.language,
                context,
            );

            if let Some(resolved_path) = resolved_path {
                let exported_name = if imp.is_default {
                    "default".to_string()
                } else {
                    imp.exported_name.clone()
                };
                let member_name = if imp.is_namespace {
                    Some(
                        reference
                            .reference_name
                            .replacen(&format!("{}.", imp.local_name), "", 1),
                    )
                } else {
                    None
                };

                let want = WantedSymbol {
                    is_default: imp.is_default,
                    is_namespace: imp.is_namespace,
                    exported_name,
                    member_name,
                };
                let mut visited: HashSet<String> = HashSet::new();
                let target_node = find_exported_symbol(
                    &resolved_path,
                    &want,
                    reference.language,
                    context,
                    &mut visited,
                    0,
                );

                if let Some(target_node) = target_node {
                    return Some(ResolvedRef {
                        original: reference.clone(),
                        target_node_id: target_node.id,
                        confidence: 0.9,
                        resolved_by: ResolvedBy::Import,
                    });
                }
            }
        }
    }

    None
}

/// Resolve a Java/Kotlin reference whose receiver is the simple name of
/// an imported FQN: `Foo.bar(...)` where `import com.example.Foo;`. The
/// imported FQN converts to a file-path suffix (`com/example/Foo.java`
/// or `.kt`) which uniquely identifies the right symbol when multiple
/// classes share the same simple name.
///
/// Also handles bare references to the imported class itself
/// (`new Foo()` extraction emits `Foo` as a `references`/`instantiates`
/// ref) and `import static <Foo>.bar` style imports of a single member.
fn resolve_java_imported_reference(
    reference: &UnresolvedRef,
    imports: &[ImportMapping],
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if imports.is_empty() {
        return None;
    }

    let ext = if reference.language == Language::Kotlin {
        ".kt"
    } else {
        ".java"
    };

    for imp in imports {
        let matches_bare = imp.local_name == reference.reference_name;
        let matches_qualified = reference
            .reference_name
            .starts_with(&format!("{}.", imp.local_name));
        if !matches_bare && !matches_qualified {
            continue;
        }

        // Convert FQN to a file-path suffix. `com.example.Foo` ->
        // `com/example/Foo.java` (or `.kt`). The actual file may live
        // under any source root (`src/main/java/`, `src/`, etc.), so match
        // by suffix rather than exact path.
        let fqn_path = format!("{}{ext}", imp.source.replace('.', "/"));

        // Which symbol name to look up: the class itself, or a member.
        let member_name = if matches_bare {
            imp.local_name.clone()
        } else {
            reference.reference_name[imp.local_name.len() + 1..].to_string()
        };

        let candidates = context.get_nodes_by_name(&member_name);
        for node in &candidates {
            if node.language != reference.language {
                continue;
            }
            let fp = node.file_path.replace('\\', "/");
            if fp.ends_with(&fqn_path) || fp.ends_with(&format!("/{fqn_path}")) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: node.id.clone(),
                    confidence: 0.9,
                    resolved_by: ResolvedBy::Import,
                });
            }
        }

        // `import static com.example.Foo.bar;` — the FQN's tail is the
        // member name, the part before is the owner class. Look up the
        // member named `<imp.localName>` (e.g. `bar`) and prefer the
        // candidate whose file matches the parent FQN's path.
        if matches_bare {
            if let Some(dot) = imp.source.rfind('.') {
                if dot > 0 {
                    let owner_fqn = &imp.source[..dot];
                    let owner_path = format!("{}{ext}", owner_fqn.replace('.', "/"));
                    for node in &candidates {
                        if node.language != reference.language {
                            continue;
                        }
                        let fp = node.file_path.replace('\\', "/");
                        if fp.ends_with(&owner_path) || fp.ends_with(&format!("/{owner_path}")) {
                            return Some(ResolvedRef {
                                original: reference.clone(),
                                target_node_id: node.id.clone(),
                                confidence: 0.9,
                                resolved_by: ResolvedBy::Import,
                            });
                        }
                    }
                }
            }
        }
    }
    None
}

/// Resolve a Go cross-package qualified reference (`pkga.FuncX`) by matching
/// the package alias against an in-module import, stripping the module prefix
/// to a project-relative directory, and locating the exported symbol in any
/// `.go` file under that directory. Returns `None` for stdlib / third-party
/// imports (no `go.mod`-relative match) so the rest of `resolve_via_import`
/// can still try the file-based path.
fn resolve_go_cross_package_reference(
    reference: &UnresolvedRef,
    imports: &[ImportMapping],
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let module = context.get_go_module()?;

    // Qualified call: receiver before `.`, member after. A bare reference
    // (no dot) is a same-file/in-package call — handled elsewhere.
    let dot_idx = reference.reference_name.find('.')?;
    if dot_idx == 0 {
        return None;
    }
    let receiver = &reference.reference_name[..dot_idx];
    let member_name = &reference.reference_name[dot_idx + 1..];
    if member_name.is_empty() {
        return None;
    }

    for imp in imports {
        if imp.local_name != receiver {
            continue;
        }
        let Some(pkg_dir) =
            go_package_dir_for_import(module, &imp.source, context.get_project_root())
        else {
            continue;
        };

        // Look up the member by name and pick the candidate whose file lives
        // directly in the package directory. Match the immediate parent dir
        // exactly so a call to `pkga.FuncX` doesn't accidentally land on a
        // `FuncX` declared in `pkga/subpkg/`.
        let candidates = context.get_nodes_by_name(member_name);
        for node in &candidates {
            if node.language != Language::Go {
                continue;
            }
            if node.is_exported != Some(true) {
                continue;
            }
            let fp = node.file_path.replace('\\', "/");
            let file_dir = match fp.rfind('/') {
                Some(last_slash) => &fp[..last_slash],
                None => "",
            };
            if file_dir == pkg_dir {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: node.id.clone(),
                    confidence: 0.9,
                    resolved_by: ResolvedBy::Import,
                });
            }
        }
    }
    None
}

/// Recursive depth cap for re-export chain following. Real codebases
/// rarely chain barrels more than 2–3 deep; 8 is a generous safety
/// net that still bounds worst-case work.
const REEXPORT_MAX_DEPTH: u32 = 8;

/// What [`find_exported_symbol`] is looking for (TS inline `want` object).
struct WantedSymbol {
    is_default: bool,
    is_namespace: bool,
    exported_name: String,
    member_name: Option<String>,
}

/// Find an exported symbol in `file_path`, following `export { x } from
/// './other'` and `export * from './other'` chains until the original
/// declaration is reached. Cycle-safe via the `visited` set.
///
/// Without this, every barrel-style import (`import { Foo } from
/// './index'` where `index.ts` only re-exports) used to resolve to
/// nothing — the existing code only looked for declarations IN the
/// resolved file, not declarations the file forwarded.
fn find_exported_symbol(
    file_path: &str,
    want: &WantedSymbol,
    language: Language,
    context: &dyn ResolutionContext,
    visited: &mut HashSet<String>,
    depth: u32,
) -> Option<Node> {
    if depth > REEXPORT_MAX_DEPTH {
        return None;
    }
    if visited.contains(file_path) {
        return None;
    }
    visited.insert(file_path.to_string());

    let nodes_in_file = context.get_nodes_in_file(file_path);

    // 1. Direct hit: the symbol is declared in this file.
    if want.is_default {
        // Svelte/Vue single-file components ARE the module's default export,
        // but are extracted as kind 'component' (not function/class). Prefer
        // the component node; fall back to an exported function/class for the
        // `.ts`/`.tsx` `export default fn`/`class` case. Without the component
        // branch, an `export { default as X } from './X.svelte'` barrel never
        // resolves and the component shows a false 0 callers (#629).
        let direct = nodes_in_file
            .iter()
            .find(|n| n.is_exported == Some(true) && n.kind == NodeKind::Component)
            .or_else(|| {
                nodes_in_file.iter().find(|n| {
                    n.is_exported == Some(true)
                        && (n.kind == NodeKind::Function || n.kind == NodeKind::Class)
                })
            });
        if let Some(direct) = direct {
            return Some(direct.clone());
        }
    } else if want.is_namespace && want.member_name.as_deref().is_some_and(|m| !m.is_empty()) {
        // (TS `want.memberName` truthiness — an empty string falls through
        // to the exported-name branch below.)
        let member_name = want.member_name.as_deref().unwrap_or("");
        let direct = nodes_in_file
            .iter()
            .find(|n| n.name == member_name && n.is_exported == Some(true));
        if let Some(direct) = direct {
            return Some(direct.clone());
        }
    } else {
        let direct = nodes_in_file
            .iter()
            .find(|n| n.name == want.exported_name && n.is_exported == Some(true));
        if let Some(direct) = direct {
            return Some(direct.clone());
        }
    }

    // 2. Re-export hit: the file forwards the symbol to another module.
    let re_exports = context.get_re_exports(file_path, language);
    if re_exports.is_empty() {
        return None;
    }

    // Look for explicit `export { want } from './other'` (with optional rename).
    let target_name = if want.is_default {
        "default"
    } else {
        &want.exported_name
    };
    for rex in &re_exports {
        if let ReExport::Named {
            exported_name,
            original_name,
            source,
        } = rex
        {
            if exported_name == target_name {
                let Some(next) = resolve_import_path(source, file_path, language, context) else {
                    continue;
                };
                // After rename: `export { foo as bar } from './x'` — to chase
                // `bar`, we look for `foo` in `./x`.
                let chained = find_exported_symbol(
                    &next,
                    &WantedSymbol {
                        is_default: original_name == "default",
                        is_namespace: false,
                        exported_name: original_name.clone(),
                        member_name: None,
                    },
                    language,
                    context,
                    visited,
                    depth + 1,
                );
                if chained.is_some() {
                    return chained;
                }
            }
        }
    }

    // 3. Wildcard re-export: `export * from './other'` — try every
    //    forwarding source. This is the barrel-of-barrels case.
    for rex in &re_exports {
        if let ReExport::Wildcard { source } = rex {
            let Some(next) = resolve_import_path(source, file_path, language, context) else {
                continue;
            };
            let chained = find_exported_symbol(&next, want, language, context, visited, depth + 1);
            if chained.is_some() {
                return chained;
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_path_helpers_match_node_semantics() {
        assert_eq!(posix_dirname("src/components/Button.ts"), "src/components");
        assert_eq!(posix_dirname("main.c"), ".");
        assert_eq!(posix_dirname("/a"), "/");
        assert_eq!(join_posix("", "main.c"), "main.c");
        assert_eq!(join_posix("/tmp/x", "src/a.ts"), "/tmp/x/src/a.ts");
        assert_eq!(
            normalize_segments("src/components/../helpers"),
            "src/helpers"
        );
        assert_eq!(normalize_segments("./../x"), "../x");
        assert_eq!(normalize_segments("/tmp/p/../../x"), "/x");
        assert_eq!(relative_posix("", "src/utils"), "src/utils");
        assert_eq!(relative_posix("/tmp/p", "/tmp/p/src/a"), "src/a");
        assert_eq!(relative_posix("/tmp/p", "/x"), "../../x");
        assert_eq!(relative_posix("/tmp/p", "/tmp/p"), "");
    }

    #[test]
    fn strip_js_comments_preserves_strings() {
        let src = "const a = \"// not a comment\"; // real comment\n/* block */ const b = 'x';";
        let out = strip_js_comments(src);
        assert!(out.contains("\"// not a comment\""));
        assert!(!out.contains("real comment"));
        assert!(!out.contains("block"));
        assert!(out.contains("const b = 'x';"));
    }

    #[test]
    fn extract_re_exports_recognises_all_forms() {
        let content = r#"
export { foo } from './a';
export { foo as bar } from './b';
export * from './c';
export * as ns from './d';
export { default as Foo } from './e';
// export { ghost } from './nope';
"#;
        let out = extract_re_exports(content, Language::Typescript);
        assert_eq!(
            out,
            vec![
                ReExport::Wildcard {
                    source: "./c".into()
                },
                ReExport::Wildcard {
                    source: "./d".into()
                },
                ReExport::Named {
                    exported_name: "foo".into(),
                    original_name: "foo".into(),
                    source: "./a".into(),
                },
                ReExport::Named {
                    exported_name: "bar".into(),
                    original_name: "foo".into(),
                    source: "./b".into(),
                },
                ReExport::Named {
                    exported_name: "Foo".into(),
                    original_name: "default".into(),
                    source: "./e".into(),
                },
            ]
        );
    }

    #[test]
    fn extract_re_exports_non_js_languages_return_empty() {
        assert!(extract_re_exports("export * from './x';", Language::Python).is_empty());
        assert!(extract_re_exports("export * from './x';", Language::Go).is_empty());
    }

    #[test]
    fn java_import_mappings_carry_fqn_and_skip_wildcards() {
        let content = r#"
package com.example.app;

// import com.example.Commented;
import com.example.dao.FooConverter;
import static com.example.util.Strings.join;
import com.example.everything.*;

public class App {}
"#;
        let mappings = extract_import_mappings("App.java", content, Language::Java);
        assert_eq!(mappings.len(), 2);
        assert_eq!(mappings[0].local_name, "FooConverter");
        assert_eq!(mappings[0].exported_name, "FooConverter");
        assert_eq!(mappings[0].source, "com.example.dao.FooConverter");
        assert_eq!(mappings[1].local_name, "join");
        assert_eq!(mappings[1].source, "com.example.util.Strings.join");
    }

    #[test]
    fn go_import_mappings_single_and_block() {
        let content = r#"
package main

import "fmt"
import alias "github.com/example/proj/pkga"

import (
    "strings"
    p2 "github.com/example/proj/pkgb"
)
"#;
        let mappings = extract_import_mappings("main.go", content, Language::Go);
        let names: Vec<(&str, &str)> = mappings
            .iter()
            .map(|m| (m.local_name.as_str(), m.source.as_str()))
            .collect();
        assert!(names.contains(&("fmt", "fmt")));
        assert!(names.contains(&("alias", "github.com/example/proj/pkga")));
        assert!(names.contains(&("strings", "strings")));
        assert!(names.contains(&("p2", "github.com/example/proj/pkgb")));
        assert!(
            mappings
                .iter()
                .all(|m| m.is_namespace && m.exported_name == "*")
        );
    }

    #[test]
    fn php_use_statements_with_alias() {
        let content = "<?php\nuse App\\Models\\User;\nuse App\\Services\\Auth as AuthService;\n";
        let mappings = extract_import_mappings("a.php", content, Language::Php);
        assert_eq!(mappings.len(), 2);
        assert_eq!(mappings[0].local_name, "User");
        assert_eq!(mappings[0].source, "App\\Models\\User");
        assert_eq!(mappings[1].local_name, "AuthService");
        assert_eq!(mappings[1].exported_name, "Auth");
    }

    #[test]
    fn js_require_statements() {
        let content = "const fs = require('fs');\nconst { a, b: c } = require('./lib');\n";
        let mappings = extract_import_mappings("a.js", content, Language::Javascript);
        assert_eq!(mappings.len(), 3);
        assert!(mappings[0].is_default && mappings[0].local_name == "fs");
        assert_eq!(mappings[1].local_name, "a");
        assert_eq!(mappings[2].local_name, "c");
        assert_eq!(mappings[2].exported_name, "b");
    }

    #[test]
    fn svelte_and_vue_reuse_js_import_extraction() {
        let content = "<script>\nimport Button from './Button.svelte';\n</script>\n<div/>";
        for lang in [Language::Svelte, Language::Vue] {
            let mappings = extract_import_mappings("App.svelte", content, lang);
            assert_eq!(mappings.len(), 1, "{lang:?}");
            assert_eq!(mappings[0].local_name, "Button");
            assert!(mappings[0].is_default);
        }
    }
}

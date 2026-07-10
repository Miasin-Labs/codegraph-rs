//! External import and standard-library detection.

use std::collections::HashSet;
use std::sync::LazyLock;

use crate::resolution::types::ResolutionContext;
use crate::resolution::workspace_packages::resolve_workspace_import;
use crate::types::Language;

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
pub(super) fn is_external_import(
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
        Language::Typescript
            | Language::Javascript
            | Language::Tsx
            | Language::Jsx
            | Language::Arkts
            | Language::Astro
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

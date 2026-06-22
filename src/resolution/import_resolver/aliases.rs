//! Path alias and workspace package import resolution.

use super::paths::extension_resolution;
use crate::resolution::path_aliases::apply_aliases;
use crate::resolution::types::ResolutionContext;
use crate::resolution::workspace_packages::resolve_workspace_import;
use crate::types::Language;

/// Resolve an aliased/absolute import.
///
/// Tries, in order:
///   1. Project-defined `compilerOptions.paths` (tsconfig/jsconfig).
///      Each pattern can have multiple replacements; tried in tsconfig
///      priority order with extension permutations.
///   2. The legacy hard-coded fallback list (`@/`, `~/`, `src/`, ...)
///      for projects that have aliases but no tsconfig paths block.
///   3. Direct path lookup (with extensions).
pub(super) fn resolve_aliased_import(
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

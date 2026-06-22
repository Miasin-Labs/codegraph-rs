mod cpp;
mod go;
mod imports;
mod path_imports;
mod re_exports;

use crate::resolution::types::{ResolutionContext, ResolvedRef, UnresolvedRef};
use crate::types::Language;

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
    imports::resolve_jvm_import(reference, context)
}

/// Resolve a reference using import mappings
pub fn resolve_via_import(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if let Some(cpp_result) = cpp::resolve_cpp_include_reference(reference, context) {
        return Some(cpp_result);
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
        if let Some(go_result) =
            go::resolve_go_cross_package_reference(reference, &imports, context)
        {
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
        if let Some(java_result) =
            imports::resolve_java_imported_reference(reference, &imports, context)
        {
            return Some(java_result);
        }
    }

    path_imports::resolve_path_import_reference(reference, &imports, context)
}

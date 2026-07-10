//! Language-specific import and re-export extraction.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use super::cpp::clear_cpp_include_dir_cache;
use crate::resolution::types::ImportMapping;
use crate::types::Language;

mod cpp;
mod go;
mod js;
mod jvm;
mod php;
mod python;
mod re_exports;

pub use re_exports::extract_re_exports;
#[cfg(test)]
pub(super) use re_exports::strip_js_comments;

/// Extract import mappings from a file
pub fn extract_import_mappings(
    _file_path: &str,
    content: &str,
    language: Language,
) -> Vec<ImportMapping> {
    let mut mappings: Vec<ImportMapping> = Vec::new();

    match language {
        Language::Typescript
        | Language::Javascript
        | Language::Tsx
        | Language::Jsx
        | Language::Arkts => {
            mappings.extend(js::extract_js_imports(content));
        }
        Language::Svelte | Language::Vue | Language::Astro => {
            // Svelte/Vue single-file components import via plain ES6 inside their
            // `<script>` block. Without this, a `.svelte`/`.vue` consumer produces
            // zero import mappings, so `resolveViaImport` can't run and a barrel
            // import (`import { Foo } from './lib'`) falls back to name-matching —
            // which silently fails whenever the re-export alias differs from the
            // component's real name, yielding a false 0 callers (#629). The ES6
            // import regex only matches `import … from '…'`, so running it over the
            // whole SFC (markup + styles included) is safe.
            mappings.extend(js::extract_js_imports(content));
        }
        Language::Python => mappings.extend(python::extract_python_imports(content)),
        Language::Go => mappings.extend(go::extract_go_imports(content)),
        Language::Java | Language::Kotlin => mappings.extend(jvm::extract_java_imports(content)),
        Language::Php => mappings.extend(php::extract_php_imports(content)),
        Language::C | Language::Cpp => mappings.extend(cpp::extract_cpp_imports(content)),
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

/// Import-mappings-per-file cache. NOTE: vestigial — the TS original
/// declared this cache but never populated it (per-file caching lives in
/// the resolver's `ResolutionContext.getImportMappings`); kept so
/// `clear_import_mapping_cache` mirrors the TS export exactly.
static IMPORT_MAPPING_CACHE: LazyLock<Mutex<HashMap<String, Vec<ImportMapping>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Clear the import mapping cache (call between indexing runs)
pub fn clear_import_mapping_cache() {
    IMPORT_MAPPING_CACHE.lock().unwrap().clear();
    clear_cpp_include_dir_cache();
}

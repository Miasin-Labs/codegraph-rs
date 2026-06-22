use std::sync::LazyLock;

use regex::Regex;

use super::mapping;
use crate::resolution::types::ImportMapping;

/// Extract C/C++ import mappings from #include directives.
///
/// #include brings all symbols from the included header into scope
/// (namespace import), so each mapping uses is_namespace: true and
/// exported_name: '*'. The local_name is set to the header's basename
/// without extension so that symbol references like `MyClass` can
/// match against any include that might provide it.
pub(super) fn extract_cpp_imports(content: &str) -> Vec<ImportMapping> {
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

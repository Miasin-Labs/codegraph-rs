use std::sync::LazyLock;

use regex::Regex;

use super::mapping;
use crate::resolution::types::ImportMapping;

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
pub(super) fn extract_java_imports(content: &str) -> Vec<ImportMapping> {
    static BLOCK_COMMENT_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)/\*.*?\*/").expect("valid regex"));
    static LINE_COMMENT_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"//[^\n]*").expect("valid regex"));
    static IMPORT_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&format!(
            r"(?m)^\s*import\s+(static\s+)?([{w}.]+(?:\.\*)?)(?:\s+as\s+([{w}]+))?\s*;?\s*$",
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
        let exported_name = fqn.split('.').next_back().unwrap_or("");
        let local_name = m.get(3).map_or(exported_name, |alias| alias.as_str());
        if local_name.is_empty() {
            continue;
        }
        mappings.push(mapping(local_name, exported_name, fqn, false, false));
    }
    mappings
}

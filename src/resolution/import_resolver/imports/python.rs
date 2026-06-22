use std::sync::LazyLock;

use regex::Regex;

use super::{W, mapping};
use crate::resolution::types::ImportMapping;

/// Extract Python import mappings
pub(super) fn extract_python_imports(content: &str) -> Vec<ImportMapping> {
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

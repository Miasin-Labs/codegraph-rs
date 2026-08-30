use std::sync::LazyLock;

use regex::Regex;

use super::{W, mapping};
use crate::resolution::types::ImportMapping;

/// Extract Python import mappings
pub(super) fn extract_python_imports(content: &str) -> Vec<ImportMapping> {
    // from X import Y — either a parenthesized list, which PEP 8 line-wrapping
    // routinely spreads across multiple physical lines
    // (`from pkg import (\n    a,\n    b as c,\n)`), or a single-line list.
    // `[^#\n]+` alone stops at the first line break, so a wrapped list silently
    // lost every name after line one — including aliased ones (#1517).
    static FROM_IMPORT_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&format!(
            r"from\s+([{w}.]+)\s+import\s+(?:\(([\s\S]*?)\)|([^#\n]+))",
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
        // Group 2 is the parenthesized (possibly line-wrapped) list; group 3
        // is the single-line form. Exactly one of the alternation branches
        // matches per statement.
        let imports = m
            .get(2)
            .or_else(|| m.get(3))
            .expect("paren or plain import list")
            .as_str();

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

use std::sync::LazyLock;

use regex::Regex;

use super::{W, mapping};
use crate::resolution::types::ImportMapping;

/// Extract Go import mappings
pub(super) fn extract_go_imports(content: &str) -> Vec<ImportMapping> {
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

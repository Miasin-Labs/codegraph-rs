use std::sync::LazyLock;

use regex::Regex;

use super::{W, mapping};
use crate::resolution::types::ImportMapping;

/// Extract PHP import mappings (use statements)
pub(super) fn extract_php_imports(content: &str) -> Vec<ImportMapping> {
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

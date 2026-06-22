use std::sync::LazyLock;

use regex::Regex;

use super::{W, mapping};
use crate::resolution::types::ImportMapping;

/// Extract JS/TS import mappings
pub(super) fn extract_js_imports(content: &str) -> Vec<ImportMapping> {
    static IMPORT_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&format!(
            r#"import\s+(?:({W}+)\s*,?\s*)?(?:\{{([^}}]+)\}})?\s*(?:(\*)\s+as\s+({W}+))?\s*from\s*['"]([^'"]+)['"]"#
        ))
        .expect("valid regex")
    });
    static NAME_ALIAS_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(&format!(r"({W}+)\s+as\s+({W}+)")).expect("valid regex"));
    static REQUIRE_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&format!(
            r#"(?:const|let|var)\s+(?:({W}+)|\{{([^}}]+)\}})\s*=\s*require\(['"]([^'"]+)['"]\)"#
        ))
        .expect("valid regex")
    });
    static DESTRUCTURE_ALIAS_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(&format!(r"({W}+)\s*:\s*({W}+)")).expect("valid regex"));

    let mut mappings: Vec<ImportMapping> = Vec::new();

    // ES6 imports
    for m in IMPORT_RE.captures_iter(content) {
        let default_import = m.get(1).map(|g| g.as_str());
        let named_imports = m.get(2).map(|g| g.as_str());
        let star = m.get(3).map(|g| g.as_str());
        let namespace_alias = m.get(4).map(|g| g.as_str());
        let source = m.get(5).expect("group 5").as_str();

        // Default import
        if let Some(default_import) = default_import {
            mappings.push(mapping(default_import, "default", source, true, false));
        }

        // Named imports
        if let Some(named_imports) = named_imports {
            for name in named_imports.split(',') {
                let name = name.trim();
                if let Some(alias) = NAME_ALIAS_RE.captures(name) {
                    mappings.push(mapping(
                        alias.get(2).expect("group 2").as_str(),
                        alias.get(1).expect("group 1").as_str(),
                        source,
                        false,
                        false,
                    ));
                } else if !name.is_empty() {
                    mappings.push(mapping(name, name, source, false, false));
                }
            }
        }

        // Namespace import
        if let (Some(_star), Some(namespace_alias)) = (star, namespace_alias) {
            mappings.push(mapping(namespace_alias, "*", source, false, true));
        }
    }

    // Require statements
    for m in REQUIRE_RE.captures_iter(content) {
        let default_name = m.get(1).map(|g| g.as_str());
        let destructured = m.get(2).map(|g| g.as_str());
        let source = m.get(3).expect("group 3").as_str();

        if let Some(default_name) = default_name {
            mappings.push(mapping(default_name, "default", source, true, false));
        }

        if let Some(destructured) = destructured {
            for name in destructured.split(',') {
                let name = name.trim();
                if let Some(alias) = DESTRUCTURE_ALIAS_RE.captures(name) {
                    mappings.push(mapping(
                        alias.get(2).expect("group 2").as_str(),
                        alias.get(1).expect("group 1").as_str(),
                        source,
                        false,
                        false,
                    ));
                } else if !name.is_empty() {
                    mappings.push(mapping(name, name, source, false, false));
                }
            }
        }
    }

    mappings
}

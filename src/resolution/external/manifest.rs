//! The two facts of a crate's `Cargo.toml` the external pass needs: the
//! library's name as code spells it, and its root file.

use std::fs;
use std::path::Path;

/// A crate's library target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LibTarget {
    /// The name code uses for the crate (`[lib] name`, else the package
    /// name with `-` as `_`).
    pub(crate) name: String,
    /// The library root, relative to the crate directory (`[lib] path`,
    /// else `src/lib.rs`).
    pub(crate) root: String,
}

impl LibTarget {
    /// Read `crate_dir/Cargo.toml`; `package` names the crate when the
    /// manifest does not (or cannot be read).
    pub(crate) fn read(crate_dir: &Path, package: &str) -> LibTarget {
        let text = fs::read_to_string(crate_dir.join("Cargo.toml")).unwrap_or_default();
        Self::parse(&text, package)
    }

    pub(crate) fn parse(manifest: &str, package: &str) -> LibTarget {
        let mut section = String::new();
        let mut lib_name = None;
        let mut lib_path = None;
        for line in manifest.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                section = line
                    .trim_matches(|c| c == '[' || c == ']')
                    .trim()
                    .to_string();
                continue;
            }
            if section != "lib" {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim().trim_matches('"').to_string();
            match key.trim() {
                "name" => lib_name = Some(value),
                "path" => lib_path = Some(value),
                _ => {}
            }
        }
        LibTarget {
            name: lib_name.unwrap_or_else(|| code_name(package)),
            root: lib_path
                .map(|path| path.trim_start_matches("./").to_string())
                .unwrap_or_else(|| "src/lib.rs".to_string()),
        }
    }
}

/// How code spells a package name (`tree-sitter` → `tree_sitter`).
pub(crate) fn code_name(package: &str) -> String {
    package.replace('-', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_library_name_and_root() {
        let lib = LibTarget::parse(
            "[package]\nname = \"tree-sitter\"\n\n[lib]\npath = \"binding_rust/lib.rs\"\n",
            "tree-sitter",
        );
        assert_eq!(lib.name, "tree_sitter");
        assert_eq!(lib.root, "binding_rust/lib.rs");
        let renamed = LibTarget::parse("[lib]\nname = \"yaml\"\n", "serde_yaml_ng");
        assert_eq!(renamed.name, "yaml");
        assert_eq!(renamed.root, "src/lib.rs");
        assert_eq!(LibTarget::parse("", "serde_json").name, "serde_json");
    }
}

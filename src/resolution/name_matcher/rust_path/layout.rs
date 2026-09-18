//! The crate/module layout, derived from file paths alone.
//!
//! Rust nodes are not stored under module paths: the module lives in the file
//! path (`src/graph/cancel.rs` is `crate::graph::cancel`) plus any inline
//! `mod` blocks, which show up as leading snake_case segments of a qualified
//! name (`tests::case`).

/// Where a file or module sits: which crate, and the module path inside it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct ModuleLocation {
    pub(super) crate_key: String,
    pub(super) module: Vec<String>,
}

impl ModuleLocation {
    /// Whether `self` is `ancestor` or nested inside it.
    pub(super) fn is_within(&self, ancestor: &ModuleLocation) -> bool {
        self.crate_key == ancestor.crate_key && self.module.starts_with(&ancestor.module)
    }

    /// The parent module, or `None` at the crate root.
    pub(super) fn parent(&self) -> Option<ModuleLocation> {
        let (_, parent) = self.module.split_last()?;
        Some(ModuleLocation {
            crate_key: self.crate_key.clone(),
            module: parent.to_vec(),
        })
    }

    /// Follow module-path segments from this module the way a 2018-edition
    /// `use` path does: a leading `crate`/`$crate` is the root, leading
    /// `self`/`super` stay/climb, and any other segment is a child module.
    /// `None` when `super` climbs past the root or a keyword appears mid-path.
    pub(super) fn walk<S: AsRef<str>>(&self, segments: &[S]) -> Option<ModuleLocation> {
        let mut module = self.module.clone();
        let mut at_head = true;
        for (index, segment) in segments.iter().enumerate() {
            match segment.as_ref() {
                "crate" | "$crate" if index == 0 => module.clear(),
                "self" if index == 0 => {}
                "super" if at_head => {
                    module.pop()?;
                }
                "crate" | "$crate" | "self" | "super" => return None,
                name => {
                    at_head = false;
                    module.push(name.to_string());
                }
            }
        }
        Some(ModuleLocation {
            crate_key: self.crate_key.clone(),
            module,
        })
    }
}

/// Map a project-relative `.rs` path to its crate and module.
///
/// `analysis/src/adapter/cpp.rs` -> crate `analysis/src`, module `adapter::cpp`;
/// `src/graph/mod.rs` -> crate `src`, module `graph`; a binary under
/// `src/bin/<name>/` is its own crate. Files outside a `src/` tree (integration
/// tests, examples) are single-file crates.
pub(super) fn module_location(file_path: &str) -> ModuleLocation {
    let normalized = file_path.replace('\\', "/");
    let parts: Vec<&str> = normalized.split('/').filter(|p| !p.is_empty()).collect();
    // The innermost `src` owns the file: `src/tools/clippy/clippy_lints/src/x.rs`
    // belongs to the clippy_lints crate, not a `tools::clippy::…` module.
    let Some(src) = parts.iter().rposition(|part| *part == "src") else {
        return ModuleLocation {
            crate_key: normalized,
            module: Vec::new(),
        };
    };
    let mut crate_parts: Vec<&str> = parts[..=src].to_vec();
    let mut rest: &[&str] = &parts[src + 1..];
    if rest.first() == Some(&"bin") && rest.len() >= 2 {
        if rest.len() == 2 {
            // src/bin/<name>.rs is a one-file binary crate.
            crate_parts.extend_from_slice(rest);
            return ModuleLocation {
                crate_key: crate_parts.join("/"),
                module: Vec::new(),
            };
        }
        crate_parts.extend_from_slice(&rest[..2]);
        rest = &rest[2..];
    }
    let mut module: Vec<String> = Vec::new();
    for (index, part) in rest.iter().enumerate() {
        if index + 1 == rest.len() {
            let stem = part.strip_suffix(".rs").unwrap_or(part);
            if !matches!(stem, "lib" | "main" | "mod") {
                module.push(stem.to_string());
            }
        } else {
            module.push((*part).to_string());
        }
    }
    ModuleLocation {
        crate_key: crate_parts.join("/"),
        module,
    }
}

/// The files that can hold a module's own items — the inverse of
/// [`module_location`]: `a/b.rs` or `a/b/mod.rs` for a module, `lib.rs` or
/// `main.rs` for a crate root, and the file itself for a one-file crate.
pub(super) fn module_files(location: &ModuleLocation) -> Vec<String> {
    let root = &location.crate_key;
    if root.ends_with(".rs") {
        return if location.module.is_empty() {
            vec![root.clone()]
        } else {
            Vec::new()
        };
    }
    if location.module.is_empty() {
        return vec![format!("{root}/lib.rs"), format!("{root}/main.rs")];
    }
    let path = location.module.join("/");
    vec![format!("{root}/{path}.rs"), format!("{root}/{path}/mod.rs")]
}

/// Inline `mod` blocks enclosing an item, read from its qualified name: the
/// leading lower-case segments (`tests::case` -> `tests`, `a::b::Type::m` ->
/// `a::b`). Rust names modules in snake_case and types in UpperCamelCase, so
/// the first capitalised segment is where the owner type begins.
pub(super) fn inline_modules(qualified_name: &str) -> Vec<String> {
    let segments: Vec<&str> = qualified_name.split("::").collect();
    let Some((_item, scope)) = segments.split_last() else {
        return Vec::new();
    };
    scope
        .iter()
        .take_while(|segment| {
            segment
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        })
        .map(|segment| (*segment).to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_files_to_crate_and_module() {
        let at = |path: &str| module_location(path);
        assert_eq!(at("src/lib.rs").module, Vec::<String>::new());
        assert_eq!(at("src/graph/cancel.rs").module, ["graph", "cancel"]);
        assert_eq!(at("src/graph/mod.rs").module, ["graph"]);
        assert_eq!(at("analysis/src/adapter/cpp.rs").crate_key, "analysis/src");
        assert_eq!(at("analysis/src/adapter/cpp.rs").module, ["adapter", "cpp"]);
        let bin = at("src/bin/codegraph/analyze/co_change.rs");
        assert_eq!(bin.crate_key, "src/bin/codegraph");
        assert_eq!(bin.module, ["analyze", "co_change"]);
        assert_eq!(at("src/bin/tool.rs").crate_key, "src/bin/tool.rs");
        assert_eq!(at("tests/api.rs").crate_key, "tests/api.rs");
        let nested = at("src/tools/clippy/clippy_lints/src/methods/chars_cmp.rs");
        assert_eq!(nested.crate_key, "src/tools/clippy/clippy_lints/src");
        assert_eq!(nested.module, ["methods", "chars_cmp"]);
    }

    #[test]
    fn module_files_invert_module_location() {
        for file in [
            "src/lib.rs",
            "src/main.rs",
            "src/graph.rs",
            "src/graph/mod.rs",
            "src/mcp/tools/registry/catalog.rs",
            "analysis/src/adapter/cpp.rs",
            "src/bin/codegraph/main.rs",
            "src/bin/codegraph/analyze/co_change.rs",
            "src/bin/tool.rs",
            "tests/api.rs",
        ] {
            let location = module_location(file);
            let files = module_files(&location);
            assert!(files.iter().any(|f| f == file), "{file} not in {files:?}");
            for candidate in &files {
                assert_eq!(module_location(candidate), location, "{candidate}");
            }
        }
        let one_file = module_location("tests/api.rs");
        let child = one_file.walk(&["helpers"]).unwrap();
        assert!(module_files(&child).is_empty());
    }

    #[test]
    fn walks_use_paths_from_a_module() {
        let here = module_location("src/mcp/tools/format.rs");
        let path = |location: Option<ModuleLocation>| location.map(|l| l.module.join("::"));
        assert_eq!(
            path(here.walk(&["budget"])),
            Some("mcp::tools::format::budget".into())
        );
        assert_eq!(
            path(here.walk(&["self", "budget"])),
            Some("mcp::tools::format::budget".into())
        );
        assert_eq!(
            path(here.walk(&["super", "super", "db"])),
            Some("mcp::db".into())
        );
        assert_eq!(path(here.walk(&["crate", "db"])), Some("db".into()));
        assert_eq!(
            path(here.walk(&["self", "super"])),
            Some("mcp::tools".into())
        );
        assert_eq!(path(here.walk(&["super"; 4])), None, "climbs past the root");
        assert_eq!(path(here.walk(&["a", "super"])), None, "super mid-path");
        assert_eq!(path(here.walk(&["a", "crate"])), None, "crate mid-path");
    }

    #[test]
    fn reads_inline_modules_from_qualified_names() {
        assert_eq!(inline_modules("tests::case"), ["tests"]);
        assert_eq!(inline_modules("outer::inner::f"), ["outer", "inner"]);
        assert_eq!(inline_modules("tests::Fixture::read"), ["tests"]);
        assert!(inline_modules("Type::method").is_empty());
        assert!(inline_modules("free_fn").is_empty());
    }
}

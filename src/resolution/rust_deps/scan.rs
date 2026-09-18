//! The public method names a crate's source defines, read with the
//! tree-sitter-rust grammar extraction already uses.
//!
//! A method is a fn taking `self` in an `impl` or `trait` block: a `pub fn`
//! of an inherent impl, any fn of a trait impl (callable wherever the trait
//! is), or an item of a `pub trait`. Private and `pub(crate)` methods cannot
//! be called from a dependent crate and are left out; so are associated fns
//! without `self` (`Type::f(..)` names its type), and `tests/`, `benches/`,
//! `examples/`, and build scripts. Methods a macro generates are not seen.
//!
//! The scan is bounded: at most [`Limits::max_files`] files and
//! [`Limits::max_bytes`] bytes, files over [`Limits::max_file_bytes`]
//! skipped, and it stops with no answer at the deadline.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::time::Instant;

use crate::extraction::grammars::create_parser;
use crate::extraction::tree_sitter_types::SyntaxNode;
use crate::types::Language;

/// Directories never read: they hold no library API.
const SKIPPED_DIRS: &[&str] = &["benches", "examples", "fuzz", "target", "tests"];

#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub(crate) max_files: usize,
    pub(crate) max_bytes: u64,
    pub(crate) max_file_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_files: 4_000,
            max_bytes: 32 * 1024 * 1024,
            max_file_bytes: 2 * 1024 * 1024,
        }
    }
}

/// Crates never reported as re-exported: `pub use` paths rooted there name
/// std or the crate itself.
const NOT_CRATES: &[&str] = &["alloc", "core", "crate", "self", "std", "super"];

/// The crates the library root of `crate_dir` re-exports items of
/// (`pub use clap_builder::*;`, `pub extern crate log;`), as code names
/// them: their methods are callable through this crate.
pub(crate) fn reexported_crates(crate_dir: &Path) -> Vec<String> {
    let Ok(source) = fs::read_to_string(crate_dir.join(lib_root(crate_dir))) else {
        return Vec::new();
    };
    let mut crates = BTreeSet::new();
    for (index, _) in source.match_indices("pub ") {
        let before = source[..index].bytes().next_back();
        if before.is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_') {
            continue;
        }
        let rest = source[index + 4..].trim_start();
        let path = rest
            .strip_prefix("use ")
            .map(|path| path.trim_start().trim_start_matches("::"))
            .or_else(|| rest.strip_prefix("extern crate "));
        let Some(path) = path else {
            continue;
        };
        let end = path
            .bytes()
            .position(|byte| !(byte.is_ascii_alphanumeric() || byte == b'_'))
            .unwrap_or(path.len());
        let root = &path[..end];
        if !root.is_empty() && !NOT_CRATES.contains(&root) {
            crates.insert(root.to_string());
        }
    }
    crates.into_iter().collect()
}

/// The library root a manifest names (`[lib] path = …`), else `src/lib.rs`.
fn lib_root(crate_dir: &Path) -> String {
    let manifest = fs::read_to_string(crate_dir.join("Cargo.toml")).unwrap_or_default();
    let mut in_lib = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_lib = line == "[lib]";
            continue;
        }
        if let Some(value) = line.strip_prefix("path").map(str::trim_start) {
            if in_lib {
                if let Some(path) = value.strip_prefix('=') {
                    return path.trim().trim_matches('"').to_string();
                }
            }
        }
    }
    "src/lib.rs".to_string()
}

/// The public method names defined under `crate_dir`, sorted, or `None`
/// when the deadline passed first.
pub(crate) fn public_method_names(
    crate_dir: &Path,
    limits: Limits,
    deadline: Instant,
) -> Option<Vec<String>> {
    let mut parser = create_parser(Language::Rust)?;
    let mut names = BTreeSet::new();
    let (mut files, mut bytes) = (0usize, 0u64);
    let walker = walkdir::WalkDir::new(crate_dir)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            entry.depth() == 0
                || !(name.starts_with('.')
                    || entry.file_type().is_dir() && SKIPPED_DIRS.contains(&name.as_ref())
                    || entry.depth() == 1 && name == "build.rs")
        });
    for entry in walker.filter_map(Result::ok) {
        if !entry.file_type().is_file() || entry.path().extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        if Instant::now() >= deadline {
            return None;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.len() > limits.max_file_bytes {
            continue;
        }
        files += 1;
        bytes += metadata.len();
        if files > limits.max_files || bytes > limits.max_bytes {
            break;
        }
        let Ok(source) = fs::read_to_string(entry.path()) else {
            continue;
        };
        if let Some(tree) = parser.parse(&source, None) {
            collect_methods(tree.root_node(), &source, &mut names);
        }
    }
    Some(names.into_iter().collect())
}

/// Add the public methods of the items under `root` to `names`.
fn collect_methods(root: SyntaxNode<'_>, source: &str, names: &mut BTreeSet<String>) {
    // Iterative: module nesting is bounded by the input, not the stack.
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        for item in children(node) {
            match item.kind() {
                "mod_item" => pending.extend(item.child_by_field_name("body")),
                "impl_item" => {
                    let trait_impl = item.child_by_field_name("trait").is_some();
                    let body = item.child_by_field_name("body");
                    for method in body.into_iter().flat_map(children) {
                        if method.kind() == "function_item"
                            && takes_self(method)
                            && (trait_impl || is_pub(method, source))
                        {
                            add_name(method, source, names);
                        }
                    }
                }
                "trait_item" if is_pub(item, source) => {
                    let body = item.child_by_field_name("body");
                    for method in body.into_iter().flat_map(children) {
                        if matches!(method.kind(), "function_item" | "function_signature_item")
                            && takes_self(method)
                        {
                            add_name(method, source, names);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn children(node: SyntaxNode<'_>) -> Vec<SyntaxNode<'_>> {
    (0..node.named_child_count() as u32)
        .filter_map(|index| node.named_child(index))
        .collect()
}

/// A plain `pub` (not `pub(crate)`, `pub(super)`, or `pub(in …)`).
fn is_pub(item: SyntaxNode<'_>, source: &str) -> bool {
    children(item).into_iter().any(|child| {
        child.kind() == "visibility_modifier"
            && child
                .utf8_text(source.as_bytes())
                .is_ok_and(|text| text.trim() == "pub")
    })
}

fn takes_self(function: SyntaxNode<'_>) -> bool {
    function
        .child_by_field_name("parameters")
        .and_then(|parameters| {
            children(parameters)
                .into_iter()
                .find(|p| p.kind() != "attribute_item")
        })
        .is_some_and(|first| first.kind() == "self_parameter")
}

fn add_name(function: SyntaxNode<'_>, source: &str, names: &mut BTreeSet<String>) {
    let name = function
        .child_by_field_name("name")
        .and_then(|name| name.utf8_text(source.as_bytes()).ok());
    if let Some(name) = name {
        names.insert(name.trim_start_matches("r#").to_string());
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{Duration, Instant};

    use super::{Limits, public_method_names};

    #[test]
    fn reads_public_methods_of_impls_and_pub_traits() {
        let dir = tempfile::tempdir().unwrap();
        let write = |path: &str, text: &str| {
            let path = dir.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        };
        write(
            "src/lib.rs",
            "pub struct Node;\n\
             impl Node {\n\
             \x20   pub fn walk(&self) -> u8 { 0 }\n\
             \x20   pub fn new() -> Self { Node }\n\
             \x20   fn private_helper(&self) {}\n\
             \x20   pub(crate) fn internal(&self) {}\n\
             }\n\
             impl Iterator for Node {\n\
             \x20   type Item = u8;\n\
             \x20   fn next(&mut self) -> Option<u8> { None }\n\
             }\n\
             pub trait Visit { fn visit_node(&mut self, n: &Node); fn with_default(self) -> Self where Self: Sized { self } }\n\
             trait Hidden { fn hidden(&self); }\n\
             mod inner { impl super::Node { pub fn child_by_field_name(&self) {} } }\n",
        );
        write("tests/it.rs", "impl X { pub fn only_in_tests(&self) {} }");
        write("build.rs", "impl X { pub fn only_in_build(&self) {} }");
        let names = public_method_names(
            dir.path(),
            Limits::default(),
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(
            names,
            [
                "child_by_field_name",
                "next",
                "visit_node",
                "walk",
                "with_default"
            ]
        );
    }

    #[test]
    fn reads_the_crates_a_facade_reexports() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/lib.rs"),
            "pub use clap_builder::*;\npub use ::clap_derive::{Parser};\npub extern crate log;\n\
             pub use crate::inner::Thing;\nuse private_dep::X;\nfn republic() {}\n",
        )
        .unwrap();
        assert_eq!(
            super::reexported_crates(dir.path()),
            ["clap_builder", "clap_derive", "log"]
        );
    }

    #[test]
    fn stops_at_the_deadline() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("lib.rs"), "impl X { pub fn f(&self) {} }").unwrap();
        assert_eq!(
            public_method_names(dir.path(), Limits::default(), Instant::now()),
            None
        );
    }
}

//! Rust path resolution: `crate::`, `self::`, `super::`, and `Self::`.
//!
//! Rust nodes are not stored under module paths. A free function's qualified
//! name is its bare name (`check`) and a method's is `Type::method`; the
//! module lives only in the file path. So `crate::graph::cancel::check` or
//! `Self::fresh_label` can never string-match a node, and every such call was
//! left unresolved — 485 of them in codegraph's own index, invisible to
//! `callers` and `impact`.
//!
//! This strategy maps the path onto the crate/module layout instead: the
//! referencing file fixes the crate and current module, `crate`/`self`/`super`
//! pick the target module, and a candidate matches when its own file sits in
//! that module (and, for `Type::item`, its qualified name is `Type::item`).
//! `Self` becomes the enclosing impl type of the referencing method.

use crate::resolution::types::{ResolutionContext, ResolvedBy, ResolvedRef, UnresolvedRef};
use crate::types::{Language, Node};

/// Where a file sits: which crate, and the module path inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ModuleLocation {
    crate_key: String,
    module: Vec<String>,
}

/// Map a project-relative `.rs` path to its crate and module.
///
/// `analysis/src/adapter/cpp.rs` -> crate `analysis/src`, module `adapter::cpp`;
/// `src/graph/mod.rs` -> crate `src`, module `graph`; a binary under
/// `src/bin/<name>/` is its own crate. Files outside a `src/` tree (integration
/// tests, examples) are single-file crates.
fn module_location(file_path: &str) -> ModuleLocation {
    let normalized = file_path.replace('\\', "/");
    let parts: Vec<&str> = normalized.split('/').filter(|p| !p.is_empty()).collect();
    let Some(src) = parts.iter().position(|part| *part == "src") else {
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

/// Split a path, dropping generic arguments (`Vec::<u8>::new` -> `Vec::new`).
fn segments(path: &str) -> Vec<&str> {
    path.split("::")
        .map(str::trim)
        .filter(|segment| !segment.is_empty() && !segment.starts_with('<'))
        .collect()
}

fn resolved(reference: &UnresolvedRef, target: &Node, confidence: f64) -> ResolvedRef {
    ResolvedRef {
        original: reference.clone(),
        target_node_id: target.id.clone(),
        confidence,
        resolved_by: ResolvedBy::QualifiedName,
    }
}

/// Pick one candidate, or none when the evidence is ambiguous. Definitions
/// that differ only by `cfg` (same file, same qualified name — e.g. unix and
/// windows variants) are one item, represented by the first.
fn unique<'a>(candidates: &'a [&'a Node]) -> Option<&'a Node> {
    let (first, rest) = candidates.split_first()?;
    rest.iter()
        .all(|node| {
            node.file_path == first.file_path && node.qualified_name == first.qualified_name
        })
        .then_some(*first)
}

/// `Self::item` -> `<enclosing impl type>::item`.
fn resolve_self(
    reference: &UnresolvedRef,
    rest: &[&str],
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let from = context.get_node_by_id(&reference.from_node_id)?;
    let (owner, _) = from.qualified_name.rsplit_once("::")?;
    let target = format!("{owner}::{}", rest.join("::"));
    let candidates = context.get_nodes_by_qualified_name(&target);
    let rust: Vec<&Node> = candidates
        .iter()
        .filter(|node| node.language == Language::Rust)
        .collect();
    // An owner name can repeat across crates; prefer the referencing crate.
    let here = module_location(&reference.file_path).crate_key;
    let same_crate: Vec<&Node> = rust
        .iter()
        .copied()
        .filter(|node| module_location(&node.file_path).crate_key == here)
        .collect();
    if let Some(node) = unique(&same_crate).or_else(|| unique(&rust)) {
        return Some(resolved(reference, node, 0.95));
    }
    // In a trait impl the method is keyed by the trait (`Default::default`),
    // not the implementing type, so the owner above can be the trait. Fall
    // back to the one associated item of that name in the same file.
    let [item] = rest else {
        return None;
    };
    let suffix = format!("::{item}");
    let local: Vec<Node> = context
        .get_nodes_in_file(&reference.file_path)
        .into_iter()
        .filter(|node| {
            node.name == *item
                && node.qualified_name.ends_with(&suffix)
                && node.qualified_name != format!("{owner}::{item}")
        })
        .collect();
    let local_refs: Vec<&Node> = local.iter().collect();
    unique(&local_refs).map(|node| resolved(reference, node, 0.8))
}

/// `crate::`/`self::`/`super::` paths, resolved against the module layout.
fn resolve_module_path(
    reference: &UnresolvedRef,
    head: &str,
    rest: &[&str],
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let here = module_location(&reference.file_path);
    let mut base: Vec<String> = match head {
        "crate" => Vec::new(),
        "self" => here.module.clone(),
        "super" => {
            let mut module = here.module.clone();
            module.pop()?;
            module
        }
        _ => return None,
    };
    // `super::super::x`
    let mut rest = rest;
    while rest.first() == Some(&"super") {
        base.pop()?;
        rest = &rest[1..];
    }
    let (&item, prefix) = rest.split_last()?;
    let candidates = context.get_nodes_by_name(item);
    let rust: Vec<&Node> = candidates
        .iter()
        .filter(|node| node.language == Language::Rust)
        .collect();
    if rust.is_empty() {
        return None;
    }

    // `module::…::item` for a free item, or `module::…::Type::item` for an
    // associated item. Try the free reading first, then the associated one.
    let matches_module = |node: &Node, module_suffix: &[&str]| {
        let location = module_location(&node.file_path);
        if location.crate_key != here.crate_key {
            return false;
        }
        let mut expected = base.clone();
        expected.extend(module_suffix.iter().map(|s| (*s).to_string()));
        location.module == expected
    };
    let free: Vec<&Node> = rust
        .iter()
        .copied()
        .filter(|node| node.qualified_name == item && matches_module(node, prefix))
        .collect();
    if let Some(node) = unique(&free) {
        return Some(resolved(reference, node, 0.95));
    }
    if let Some((&owner, module_suffix)) = prefix.split_last() {
        let wanted = format!("{owner}::{item}");
        let associated: Vec<&Node> = rust
            .iter()
            .copied()
            .filter(|node| node.qualified_name == wanted && matches_module(node, module_suffix))
            .collect();
        if let Some(node) = unique(&associated) {
            return Some(resolved(reference, node, 0.95));
        }
    }

    // Re-exports (`pub use`) move items away from the module their path
    // names. When the module match fails, accept the one same-crate item of
    // the right shape; for `crate::item`, also the one such item project-wide
    // (a re-export from another workspace crate). Never guess among several.
    let shape: Vec<&Node> = rust
        .iter()
        .copied()
        .filter(|node| match prefix.last() {
            Some(owner) if owner.chars().next().is_some_and(char::is_uppercase) => {
                node.qualified_name == format!("{owner}::{item}")
            }
            _ => node.qualified_name == item,
        })
        .collect();
    let same_crate: Vec<&Node> = shape
        .iter()
        .copied()
        .filter(|node| module_location(&node.file_path).crate_key == here.crate_key)
        .collect();
    if let Some(node) = unique(&same_crate) {
        return Some(resolved(reference, node, 0.8));
    }
    if head == "crate" && same_crate.is_empty() {
        if let Some(node) = unique(&shape) {
            return Some(resolved(reference, node, 0.7));
        }
    }
    None
}

/// Resolve a Rust path whose head is `crate`, `self`, `super`, or `Self`.
pub fn match_rust_path(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if reference.language != Language::Rust || !reference.reference_name.contains("::") {
        return None;
    }
    let parts = segments(&reference.reference_name);
    let (&head, rest) = parts.split_first()?;
    if rest.is_empty() {
        return None;
    }
    match head {
        "Self" => resolve_self(reference, rest, context),
        "crate" | "self" | "super" => resolve_module_path(reference, head, rest, context),
        _ => None,
    }
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
    }

    #[test]
    fn strips_generic_arguments_from_segments() {
        assert_eq!(segments("Vec::<u8>::new"), ["Vec", "new"]);
        assert_eq!(segments("crate::a::b"), ["crate", "a", "b"]);
    }
}

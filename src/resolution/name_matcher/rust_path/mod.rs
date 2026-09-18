//! Rust path resolution: `crate::`, `self::`, `super::`, and `Self::`.
//!
//! Rust nodes are not stored under module paths. A free function's qualified
//! name is its bare name (`check`) and a method's is `Type::method`; the
//! module lives only in the file path. So `crate::graph::cancel::check` or
//! `Self::fresh_label` can never string-match a node, and every such call was
//! left unresolved — 485 of them in codegraph's own index, invisible to
//! `callers` and `impact`.
//!
//! This strategy maps the path onto the crate/module layout instead
//! ([`layout`]): the referencing file fixes the crate and current module,
//! `crate`/`self`/`super` pick the target module, and a candidate matches when
//! its own file sits in that module (and, for `Type::item`, its qualified name
//! is `Type::item`). When the item lives elsewhere, the module's `pub use`
//! re-exports are followed to it ([`module_tree`], reading declarations
//! parsed by [`use_tree`]). `Self` becomes the enclosing impl type of the
//! referencing method.

mod layout;
mod module_tree;
mod use_tree;

use layout::{inline_modules, module_location};
use module_tree::{Namespace, Resolution, resolve_in_module};
pub use use_tree::{
    RustUse,
    UseBinding,
    UseLeaf,
    UseVisibility,
    parse_use_leaves,
    rust_fn_local_uses,
    rust_use_leaves,
};

use crate::resolution::types::{ResolutionContext, ResolvedBy, ResolvedRef, UnresolvedRef};
use crate::types::{Language, Node};

/// The crate a `.rs` file belongs to (see [`layout::module_location`]).
pub(in crate::resolution::name_matcher) fn crate_key(file_path: &str) -> String {
    module_location(file_path).crate_key
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

fn starts_uppercase(segment: &str) -> bool {
    segment.chars().next().is_some_and(char::is_uppercase)
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
    path: &[&str],
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let here = module_location(&reference.file_path);
    let (&item, module_path) = path.split_last()?;
    // `self`/`super` start from the current module: the file's module plus
    // any inline `mod` blocks around the caller — `super::f` inside
    // `mod tests { … }` in lib.rs means the crate root, not "above the crate
    // root". `crate` ignores where it starts.
    let mut start = here.clone();
    if module_path.first() != Some(&"crate") {
        if let Some(from) = context.get_node_by_id(&reference.from_node_id) {
            start.module.extend(inline_modules(&from.qualified_name));
        }
    }
    // The module the path names — or, for `Type::item`, the type's "module".
    let target = start.walk(module_path)?;

    // Namespace filter: a call targets a value, so modules, imports, and file
    // nodes sharing the name are not candidates (they made `crate::…::tools`
    // look ambiguous next to the `tools` fn).
    let namespace = Namespace::of(reference.reference_kind);
    let candidates = context.get_nodes_by_name(item);
    let rust: Vec<&Node> = candidates
        .iter()
        .filter(|node| node.language == Language::Rust && namespace.admits(node.kind))
        .collect();

    // `module::…::item` for a free item, or `module::…::Type::item` for an
    // associated item. Try the free reading first, then the associated one.
    let free: Vec<&Node> = rust
        .iter()
        .copied()
        .filter(|node| node.qualified_name == item && module_location(&node.file_path) == target)
        .collect();
    if let Some(node) = unique(&free) {
        return Some(resolved(reference, node, 0.95));
    }
    let owner = module_path
        .last()
        .copied()
        .filter(|owner| starts_uppercase(owner));
    if let (Some(owner), Some(owner_module)) = (owner, target.parent()) {
        let wanted = format!("{owner}::{item}");
        let associated: Vec<&Node> = rust
            .iter()
            .copied()
            .filter(|node| {
                node.qualified_name == wanted && module_location(&node.file_path) == owner_module
            })
            .collect();
        if let Some(node) = unique(&associated) {
            return Some(resolved(reference, node, 0.95));
        }
    }

    // Re-exports (`pub use`) move items away from the module their path
    // names: follow the module's use declarations to the definition. An
    // ambiguous re-export stops here rather than guessing below.
    if owner.is_none() {
        match resolve_in_module(context, &target, item, namespace) {
            Resolution::Found(node) => return Some(resolved(reference, &node, 0.95)),
            Resolution::Ambiguous => return None,
            Resolution::NotFound => {}
        }
    }

    // Last resort, for re-exports the index cannot follow (another crate,
    // a re-exported type's methods): accept the one same-crate item of the
    // right shape; for `crate::…`, also the one such item project-wide (a
    // re-export from another workspace crate). Never guess among several.
    let shape: Vec<&Node> = rust
        .iter()
        .copied()
        .filter(|node| match owner {
            Some(owner) => node.qualified_name == format!("{owner}::{item}"),
            None => node.qualified_name == item,
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
    if module_path.first() == Some(&"crate") && same_crate.is_empty() {
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
        "crate" | "self" | "super" => resolve_module_path(reference, &parts, context),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_generic_arguments_from_segments() {
        assert_eq!(segments("Vec::<u8>::new"), ["Vec", "new"]);
        assert_eq!(segments("crate::a::b"), ["crate", "a", "b"]);
    }
}

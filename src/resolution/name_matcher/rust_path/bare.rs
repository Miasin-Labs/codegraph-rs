//! What a bare Rust name (`Node`, `helper`, `Result`) means where it is
//! written, the way rustc resolves a single-segment path: a `use` inside
//! the enclosing fn first, then the module's own items and single imports,
//! then its globs ([`resolve_in_module`]).
//!
//! The name rules pick among same-named definitions by nearness, which is
//! wrong whenever a `use` says otherwise: `use tree_sitter::Node;` makes
//! `Node` tree-sitter's even when the project defines three `Node`s, and
//! `use crate::error::Result;` picks `error.rs`'s alias over a nearer
//! `calendar.rs` one. So a name the module binds decides its target here:
//! the one project item it binds, or nothing when it binds another crate's
//! item or several items. A name nothing binds (a prelude type, a generic,
//! a local, a macro-generated item) is left to the name rules.

use super::layout::item_location;
use super::module_tree::{
    Namespace,
    Resolution,
    is_external_root,
    macro_level_uses,
    resolve_in_module,
    use_source,
};
use super::use_tree::{LocalUse, UseBinding, UseLeaf};
use super::{ModuleLocation, caller_module, fn_uses_in_scope, module_location, unique};
use crate::resolution::types::{ResolutionContext, UnresolvedRef};
use crate::types::{Language, Node};

/// What a bare name binds where a reference writes it.
#[derive(Debug, Clone)]
pub(in crate::resolution::name_matcher) enum BareBinding {
    /// Nothing in scope binds it by name: a prelude item, a local or a
    /// generic parameter, an item a macro generates, or an item the index
    /// places where the module tree cannot see it.
    Unbound,
    /// A `use` binds it to an item of another crate (`use axum::extract::State`).
    External,
    /// Several distinct items could be meant: `cfg` alternatives, whichever
    /// the build picks (none when the lookup could not tell).
    Ambiguous(Vec<Node>),
    /// The one project item it binds; `imported` when a `use` brought it in.
    Item { node: Box<Node>, imported: bool },
}

/// What the bare name of `reference` binds where it is written. Only Rust
/// references named by one identifier are looked up; any other is
/// [`BareBinding::Unbound`].
pub(in crate::resolution::name_matcher) fn bare_binding(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> BareBinding {
    let name = reference.reference_name.as_str();
    if reference.language != Language::Rust || !is_plain_identifier(name) {
        return BareBinding::Unbound;
    }
    let namespace = Namespace::of(reference.reference_kind);
    let Some(caller) = reference_module(reference, context) else {
        return BareBinding::Unbound;
    };

    // A `use` in a fn holding the reference shadows the module's names.
    let fn_local: Vec<LocalUse> = context
        .get_rust_fn_local_uses(&reference.file_path)
        .iter()
        .filter(|local| binds(&local.leaf, name))
        .cloned()
        .collect();
    if !fn_local.is_empty() {
        let in_scope = fn_uses_in_scope(reference, &fn_local, context);
        if !in_scope.is_empty() {
            return leaves_binding(&in_scope, &caller, context, namespace);
        }
    }

    // The file's own `use`s binding the name decide it: they are the
    // module's, and a binary's `main.rs` shares its module path with the
    // library's `lib.rs`, whose `use`s it does not see.
    let own = module_leaves(reference, &caller, context, name);
    if !own.is_empty() {
        return leaves_binding(&own, &caller, context, namespace);
    }

    match resolve_in_module(context, &caller, name, namespace) {
        Resolution::Found(node) if other_crate_root(&reference.file_path, &node.file_path) => {
            BareBinding::Unbound
        }
        Resolution::Found(node) => BareBinding::Item {
            imported: node.file_path != reference.file_path,
            node,
        },
        Resolution::Ambiguous(nodes) => BareBinding::Ambiguous(nodes),
        Resolution::External => BareBinding::External,
        Resolution::NotFound => BareBinding::Unbound,
    }
}

/// The item `use` leaves name when the module tree cannot follow them —
/// the defining module re-exports it from inside a macro call (`cfg_rt! {
/// pub use self::builder::Builder; }`), which the index does not read: the
/// one free item of that name in the module the leaf's path names or a
/// child of it. (Deeper, the macro may hide the item itself.)
fn hidden_reexport(
    leaves: &[UseLeaf],
    caller: &ModuleLocation,
    context: &dyn ResolutionContext,
    namespace: Namespace,
) -> BareBinding {
    let mut candidates: Vec<Node> = Vec::new();
    for leaf in leaves {
        if is_external_root(context, caller, &leaf.path) {
            continue;
        }
        let Some((original, path)) = leaf.path.split_last() else {
            continue;
        };
        let Some(source) = use_source(context, caller, path) else {
            continue;
        };
        candidates.extend(
            context
                .get_nodes_by_name(original)
                .into_iter()
                .filter(|node| {
                    let (location, local) = item_location(node);
                    node.language == Language::Rust
                        && namespace.admits(node.kind)
                        && local == original.as_str()
                        && location.is_within_child_of(&source)
                }),
        );
    }
    let refs: Vec<&Node> = candidates.iter().collect();
    match unique(&refs) {
        Some(node) => BareBinding::Item {
            node: Box::new(node.clone()),
            imported: true,
        },
        None => BareBinding::Unbound,
    }
}

/// `found` is `lib.rs` or `main.rs` beside `file`, the other of the two:
/// the roots of two crates the layout gives one module path.
fn other_crate_root(file: &str, found: &str) -> bool {
    let (Some((dir, name)), Some((found_dir, found_name))) =
        (file.rsplit_once('/'), found.rsplit_once('/'))
    else {
        return false;
    };
    dir == found_dir
        && matches!(
            (name, found_name),
            ("main.rs", "lib.rs") | ("lib.rs", "main.rs")
        )
}

/// What the `use` leaves `leaves`, all binding the reference's name where
/// it is written, bind it to.
fn leaves_binding(
    leaves: &[UseLeaf],
    caller: &ModuleLocation,
    context: &dyn ResolutionContext,
    namespace: Namespace,
) -> BareBinding {
    let mut found: Vec<Node> = Vec::new();
    for leaf in leaves {
        if is_external_root(context, caller, &leaf.path) {
            return BareBinding::External;
        }
        let Some((original, path)) = leaf.path.split_last() else {
            continue;
        };
        let Some(source) = use_source(context, caller, path) else {
            continue;
        };
        match resolve_in_module(context, &source, original, namespace) {
            Resolution::Found(node) => {
                let seen = found.iter().any(|known| {
                    known.file_path == node.file_path && known.qualified_name == node.qualified_name
                });
                if !seen {
                    found.push(*node);
                }
            }
            Resolution::Ambiguous(nodes) if nodes.is_empty() => {
                return BareBinding::Ambiguous(nodes);
            }
            Resolution::Ambiguous(nodes) => found.extend(nodes),
            Resolution::External => return BareBinding::External,
            Resolution::NotFound => {}
        }
    }
    match found.len() {
        0 => hidden_reexport(leaves, caller, context, namespace),
        1 => BareBinding::Item {
            node: Box::new(found.remove(0)),
            imported: true,
        },
        _ => BareBinding::Ambiguous(found),
    }
}

/// The module the reference is written in: its file's module plus the
/// inline `mod` blocks around the referencing item. `None` when the
/// referencing node is not an item of the file (a route a framework
/// synthesized, named after its file).
fn reference_module(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ModuleLocation> {
    let caller = caller_module(reference, context);
    let file_depth = module_location(&reference.file_path).module.len();
    caller.module[file_depth..]
        .iter()
        .all(|segment| is_plain_identifier(segment))
        .then_some(caller)
}

/// The use leaves binding `name` at the top level of the reference's
/// module (the file's, at the caller's inline `mod` depth).
fn module_leaves(
    reference: &UnresolvedRef,
    caller: &ModuleLocation,
    context: &dyn ResolutionContext,
    name: &str,
) -> Vec<UseLeaf> {
    let file_depth = module_location(&reference.file_path).module.len();
    let inline = caller.module.get(file_depth..).unwrap_or_default();
    let mut leaves: Vec<UseLeaf> = context
        .get_rust_use_leaves(&reference.file_path)
        .iter()
        .filter(|found| found.inline_modules == inline && binds(&found.leaf, name))
        .map(|found| found.leaf.clone())
        .collect();
    if inline.is_empty() {
        leaves.extend(
            macro_level_uses(context, &reference.file_path)
                .iter()
                .filter(|leaf| binds(leaf, name))
                .cloned(),
        );
    }
    leaves
}

fn binds(leaf: &UseLeaf, name: &str) -> bool {
    matches!(&leaf.binding, UseBinding::Name(bound) if bound == name)
}

fn is_plain_identifier(name: &str) -> bool {
    name.bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && !matches!(name, "Self" | "self" | "super" | "crate")
}

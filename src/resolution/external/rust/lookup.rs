//! Finding an item inside one reachable crate's graph.
//!
//! * A path (`serde_json::from_str`, `tree_sitter::Node::new`) is resolved
//!   the way the crate itself would resolve `crate::…` from its library
//!   root: the module layout and its `pub use` re-exports
//!   ([`match_rust_path`] over the graph). A re-export of another crate
//!   (`pub use clap_builder::*`) is followed into that crate's graph when
//!   the project reaches it. As a last resort the one item of the path's
//!   shape in the crate is taken — never one of several.
//! * A method is `Owner::method` on the type the receiver was inferred to
//!   have — exactly one such method in the crate, or nothing.
//!
//! Only what the dependent crate could name is admitted: a private item
//! is not, except a method of a trait or a trait impl (callable wherever
//! the trait is, though the index records it without `pub`).

use std::rc::Rc;

pub(crate) use super::types::type_path;
use super::types::{aliased_type, deref_target};
use super::visibility::{admitted, overloaded, unique, visible_outside};
use crate::resolution::external::open::{ForeignGraph, GraphCache, Located};
use crate::resolution::name_matcher::{UseBinding, UseVisibility, match_rust_path};
use crate::resolution::types::{ResolutionContext, UnresolvedRef};
use crate::types::{EdgeKind, Language, Node, NodeKind, Visibility};

/// How many crate-to-crate re-export hops a path follows (`clap` →
/// `clap_builder`).
const MAX_REEXPORT_HOPS: u8 = 2;
/// How far above a method its `impl`/`trait` header is looked for.
pub(super) const MAX_HEADER_LINES: usize = 5_000;
/// How many type-alias / `Deref` hops a method lookup follows
/// (`StableDiGraph` → `StableGraph`, `CachedStatement` → `Statement`).
const MAX_TYPE_HOPS: u8 = 3;

/// An item found in a graph, and how sure the lookup is.
pub(crate) struct Found {
    pub(crate) graph: Rc<ForeignGraph>,
    pub(crate) node: Node,
    pub(crate) confidence: f64,
    /// Reached through another crate's re-export.
    pub(crate) reexported: bool,
}

/// The item `rest` names inside the crate code calls `krate`
/// (`["Node", "new"]` in `tree_sitter`), admitted for a reference of
/// `kind`.
pub(crate) fn lookup_path(
    cache: &GraphCache<'_>,
    krate: &str,
    rest: &[String],
    kind: EdgeKind,
) -> Option<Found> {
    let key = (krate.to_string(), rest.to_vec(), kind);
    let known = cache.memo.paths.borrow().get(&key).cloned();
    let located = match known {
        Some(located) => located,
        None => {
            let located = lookup_path_hops(cache, krate, rest, kind, 0).map(|found| Located {
                graph: found.graph.index,
                node: found.node,
                confidence: found.confidence,
                reexported: found.reexported,
            });
            cache.memo.paths.borrow_mut().insert(key, located.clone());
            located
        }
    }?;
    Some(Found {
        graph: cache.get_index(located.graph)?,
        node: located.node,
        confidence: located.confidence,
        reexported: located.reexported,
    })
}

fn lookup_path_hops(
    cache: &GraphCache<'_>,
    krate: &str,
    rest: &[String],
    kind: EdgeKind,
    hops: u8,
) -> Option<Found> {
    if rest.is_empty() {
        return None;
    }
    let graph = cache.get(krate)?;
    if let Some(found) = by_layout(cache, &graph, rest, kind) {
        return Some(found);
    }
    if hops < MAX_REEXPORT_HOPS {
        if let Some(mut found) = through_reexport(cache, &graph, rest, kind, hops) {
            found.reexported = true;
            return Some(found);
        }
    }
    by_shape(cache, &graph, rest, kind)
}

/// `crate::<rest>` resolved from the crate's library root.
fn by_layout(
    cache: &GraphCache<'_>,
    graph: &Rc<ForeignGraph>,
    rest: &[String],
    kind: EdgeKind,
) -> Option<Found> {
    let reference = UnresolvedRef {
        from_node_id: String::new(),
        reference_name: format!("crate::{}", rest.join("::")),
        reference_kind: kind,
        line: 1,
        column: 0,
        file_path: graph.lib_root.clone(),
        language: Language::Rust,
        candidates: None,
        metadata: None,
    };
    let resolved = match_rust_path(&reference, &graph.context)?;
    let node = graph.context.get_node_by_id(&resolved.target_node_id)?;
    (admitted(cache, graph, &node, kind) && !overloaded(cache, graph, &node)).then(|| Found {
        graph: Rc::clone(graph),
        node,
        confidence: resolved.confidence,
        reexported: false,
    })
}

/// The crate's library root re-exports another crate the project reaches:
/// `pub use clap_builder::*;` or `pub use clap_builder::Parser;`.
fn through_reexport(
    cache: &GraphCache<'_>,
    graph: &Rc<ForeignGraph>,
    rest: &[String],
    kind: EdgeKind,
    hops: u8,
) -> Option<Found> {
    let uses = graph.context.get_rust_use_leaves(&graph.lib_root);
    let (first, tail) = rest.split_first()?;
    for found in uses.iter() {
        let leaf = &found.leaf;
        if leaf.vis != UseVisibility::Public || !found.inline_modules.is_empty() {
            continue;
        }
        let Some((root, inner)) = leaf.path.split_first() else {
            continue;
        };
        if root == &graph.krate
            || matches!(root.as_str(), "crate" | "self" | "super")
            || !cache.knows(root)
        {
            continue;
        }
        let target: Vec<String> = match &leaf.binding {
            UseBinding::Glob => inner.iter().chain(rest.iter()).cloned().collect(),
            UseBinding::Name(bound) if bound == first => {
                inner.iter().chain(tail.iter()).cloned().collect()
            }
            _ => continue,
        };
        if let Some(found) = lookup_path_hops(cache, root, &target, kind, hops + 1) {
            return Some(found);
        }
    }
    None
}

/// The one item of the crate with the path's shape: `item`, or
/// `Owner::item` when the segment before it names a type.
fn by_shape(
    cache: &GraphCache<'_>,
    graph: &Rc<ForeignGraph>,
    rest: &[String],
    kind: EdgeKind,
) -> Option<Found> {
    let (item, before) = rest.split_last()?;
    let owned = before
        .last()
        .is_some_and(|owner| owner.starts_with(|c: char| c.is_uppercase()));
    let shape = match before.last() {
        Some(owner) if owned => format!("{owner}::{item}"),
        _ => item.clone(),
    };
    let candidates: Vec<Node> = graph
        .context
        .get_nodes_by_name(item)
        .into_iter()
        .filter(|node| {
            node.qualified_name == shape
                && graph.in_crate(&node.file_path)
                && admitted(cache, graph, node, kind)
        })
        .collect();
    let node = if owned {
        in_owner_file(cache, graph, before, candidates)?
    } else {
        unique(graph, candidates)?
    };
    Some(Found {
        graph: Rc::clone(graph),
        node,
        confidence: 0.75,
        reexported: false,
    })
}

/// The method `method` of the type at `owner` (its path in the crate, the
/// type's name last) — exactly one, callable from outside the crate.
/// Several types of one name (`regex::Regex`, `regex::bytes::Regex`) are
/// told apart by the file the owner's path resolves to.
pub(crate) fn lookup_method(
    cache: &GraphCache<'_>,
    graph: &Rc<ForeignGraph>,
    owner: &[String],
    method: &str,
) -> Option<Node> {
    let key = (graph.index, owner.join("::"), method.to_string());
    if let Some(known) = cache.memo.methods.borrow().get(&key) {
        return known.clone();
    }
    let found = find_method(cache, graph, owner, method, 0);
    cache.memo.methods.borrow_mut().insert(key, found.clone());
    found
}

/// `owner::method`, else — when the type itself has no such method — on
/// what it stands for: an alias's aliased type, a `Deref` impl's `Target`
/// (method calls auto-deref).
fn find_method(
    cache: &GraphCache<'_>,
    graph: &Rc<ForeignGraph>,
    owner_path: &[String],
    method: &str,
    hops: u8,
) -> Option<Node> {
    let candidates = methods_named(cache, graph, owner_path, method)?;
    if !candidates.is_empty() {
        return in_owner_file(cache, graph, owner_path, candidates);
    }
    if hops >= MAX_TYPE_HOPS {
        return None;
    }
    let (krate, path) = aliased_type(cache, graph, owner_path)
        .or_else(|| deref_target(cache, graph, owner_path))?;
    let target = cache.get(&krate)?;
    find_method(cache, &target, &path, method, hops + 1)
}

/// The methods `Owner::method` (the owner's name, any module) of the crate
/// callable from outside it.
fn methods_named(
    cache: &GraphCache<'_>,
    graph: &Rc<ForeignGraph>,
    owner_path: &[String],
    method: &str,
) -> Option<Vec<Node>> {
    let owner = owner_path.last()?;
    let exact = format!("{owner}::{method}");
    let suffix = format!("::{exact}");
    let candidates: Vec<Node> = graph
        .context
        .get_nodes_by_name(method)
        .into_iter()
        .filter(|node| {
            node.language == Language::Rust
                && node.kind == NodeKind::Method
                && (node.qualified_name == exact || node.qualified_name.ends_with(&suffix))
                && graph.in_crate(&node.file_path)
                && visible_outside(cache, graph, node)
        })
        .collect();
    Some(candidates)
}

/// One of `candidates` (members named `Owner::member`): the only one, or
/// the only one in the file where the type the owner path names is
/// defined (inherent impls sit beside their type).
fn in_owner_file(
    cache: &GraphCache<'_>,
    graph: &Rc<ForeignGraph>,
    owner_path: &[String],
    candidates: Vec<Node>,
) -> Option<Node> {
    let first = candidates.first()?;
    let one_place = candidates.iter().all(|node| {
        node.qualified_name == first.qualified_name && node.file_path == first.file_path
    });
    if one_place {
        return unique(graph, candidates);
    }
    let owner = lookup_path(cache, &graph.krate, owner_path, EdgeKind::References)?;
    if owner.graph.index != graph.index
        || !matches!(
            owner.node.kind,
            NodeKind::Struct | NodeKind::Enum | NodeKind::Union | NodeKind::Trait
        )
    {
        return None;
    }
    let here: Vec<Node> = candidates
        .into_iter()
        .filter(|node| node.file_path == owner.node.file_path)
        .collect();
    unique(graph, here)
}

/// The public field `owner.field` of the crate, when exactly one.
pub(crate) fn lookup_field(
    cache: &GraphCache<'_>,
    graph: &Rc<ForeignGraph>,
    owner: &str,
    field: &str,
) -> Option<Node> {
    let key = (graph.index, owner.to_string(), field.to_string());
    if let Some(known) = cache.memo.fields.borrow().get(&key) {
        return known.clone();
    }
    let exact = format!("{owner}::{field}");
    let candidates: Vec<Node> = graph
        .context
        .get_nodes_by_name(field)
        .into_iter()
        .filter(|node| {
            node.language == Language::Rust
                && node.kind == NodeKind::Field
                && node.qualified_name == exact
                && node.visibility == Some(Visibility::Public)
                && graph.in_crate(&node.file_path)
        })
        .collect();
    let found = unique(graph, candidates);
    cache.memo.fields.borrow_mut().insert(key, found.clone());
    found
}

/// A type (struct, enum, union, trait, alias) named `name` the crate
/// defines and exports.
pub(crate) fn defines_type(cache: &GraphCache<'_>, graph: &Rc<ForeignGraph>, name: &str) -> bool {
    let key = (graph.index, name.to_string());
    if let Some(known) = cache.memo.types.borrow().get(&key) {
        return *known;
    }
    let defined = graph.context.get_nodes_by_name(name).iter().any(|node| {
        node.language == Language::Rust
            && matches!(
                node.kind,
                NodeKind::Struct
                    | NodeKind::Enum
                    | NodeKind::Union
                    | NodeKind::Trait
                    | NodeKind::TypeAlias
            )
            && graph.in_crate(&node.file_path)
            && visible_outside(cache, graph, node)
    });
    cache.memo.types.borrow_mut().insert(key, defined);
    defined
}

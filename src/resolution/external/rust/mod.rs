//! Rust references into reachable graphs.
//!
//! What a reference names decides where it is looked up — never a name
//! shared by chance:
//!
//! - a path (`serde_json::from_str`, `Node::new` after `use
//!   tree_sitter::Node`, `linkscope::TraceField::text`), or a bare name a
//!   `use` brings in (`from_str`, `Serialize` in an `impl`), goes to the
//!   crate its first segment names ([`lookup::lookup_path`]);
//! - `recv.m(..)` and `a.b().m(..)` go to `Type::m` of the crate whose type
//!   the receiver was inferred to have, the chain typed through dependency
//!   return types where the project's own types end
//!   ([`crate::resolution::ForeignTypes`]).
//!
//! A local closure called bare, `self.m()`, a receiver of unknown type, and
//! a crate no graph is reachable for resolve to nothing.

pub(crate) mod api;
pub(crate) mod lookup;
mod prelude;
pub(crate) mod scope;
mod types;
mod visibility;

use lookup::{Found, lookup_method, lookup_path};

use super::context::ExternalContext;
use super::declarations::Declarations;
use super::names::MethodNames;
use crate::resolution::name_matcher::external::{
    CrateType,
    RustCallShape,
    dropped_receiver_crate_type,
    external_path,
    receiver_crate_type,
    rust_call_shape,
};
use crate::resolution::types::{ResolutionContext as _, UnresolvedRef};
use crate::types::{EdgeKind, Language, receiver_was_dropped};

/// How a reference was resolved into another graph (the edge's
/// `resolved_by`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExternalResolvedBy {
    /// A path naming the crate (`serde_json::from_str`).
    QualifiedName,
    /// A bare name a `use` brings in (`from_str`, `Serialize`).
    Import,
    /// A path re-exported by another crate (`clap::Parser` → `clap_builder`).
    ReExport,
    /// `recv.m(..)`, `recv` typed from its binding.
    InstanceMethod,
    /// `a.b().m(..)`, the receiver chain typed through project types only.
    ReceiverChain,
    /// A receiver whose type came through a dependency's declared return
    /// (or field) type.
    DependencyChain,
    /// A bare name of the standard prelude (`Ok`, `Vec`, `Clone`).
    Prelude,
}

impl ExternalResolvedBy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::QualifiedName => "qualified-name",
            Self::Import => "import",
            Self::ReExport => "re-export",
            Self::InstanceMethod => "instance-method",
            Self::ReceiverChain => "receiver-chain",
            Self::DependencyChain => "dependency-chain",
            Self::Prelude => "prelude",
        }
    }
}

/// A Rust reference resolved into another graph.
pub(crate) struct Resolved {
    pub(crate) found: Found,
    pub(crate) by: ExternalResolvedBy,
}

/// What one reference came to.
pub(crate) enum Attempt {
    Resolved(Box<Resolved>),
    /// Its path or receiver type is in a reachable crate, but no single
    /// item there answers it (`krate::path`, `krate::Type::method`).
    Missed(String),
    /// Nothing points it at a reachable crate.
    NotOurs,
}

/// Resolve one Rust reference into the reachable graphs.
pub(crate) fn resolve(
    reference: &UnresolvedRef,
    context: &ExternalContext<'_>,
    names: &MethodNames,
) -> Attempt {
    if reference.language != Language::Rust {
        return Attempt::NotOurs;
    }
    match reference.reference_kind {
        EdgeKind::Calls => resolve_call(reference, context, names),
        EdgeKind::References | EdgeKind::Implements | EdgeKind::Extends => {
            if reference.reference_name.contains('.') {
                return Attempt::NotOurs;
            }
            if let Some((krate, rest)) = placed(reference, context) {
                return path_item(reference, context, &krate, &rest);
            }
            // `fmt::Result` recorded as `Result`: the path as written.
            if let Some(qualifier) = prelude::written_qualifier(reference, context) {
                if qualifier.is_empty() {
                    return Attempt::NotOurs;
                }
                let written = format!("{qualifier}::{}", reference.reference_name);
                return match placed_as(&written, reference, context) {
                    Some((krate, rest)) => path_item(reference, context, &krate, &rest),
                    None => Attempt::NotOurs,
                };
            }
            prelude_name(reference, context)
        }
        _ => Attempt::NotOurs,
    }
}

/// A bare name no `use` places: the std prelude's item of that name, when
/// the file cannot mean anything else.
fn prelude_name(reference: &UnresolvedRef, context: &ExternalContext<'_>) -> Attempt {
    if !api::is_prelude_name(context.declarations.cache(), &reference.reference_name) {
        return Attempt::NotOurs;
    }
    match prelude::prelude_item(reference, context) {
        Some(found) => Attempt::Resolved(Box::new(Resolved {
            found,
            by: ExternalResolvedBy::Prelude,
        })),
        None => Attempt::NotOurs,
    }
}

/// The reachable crate and the path inside it the reference's name
/// spells, `use` declarations followed — a cheap test most references
/// fail before anything else is read.
fn placed(
    reference: &UnresolvedRef,
    context: &ExternalContext<'_>,
) -> Option<(String, Vec<String>)> {
    placed_as(&reference.reference_name, reference, context)
}

/// [`placed`] for the path `name`, as written at the reference.
fn placed_as(
    name: &str,
    reference: &UnresolvedRef,
    context: &ExternalContext<'_>,
) -> Option<(String, Vec<String>)> {
    let (krate, rest) = external_path(name, &reference.file_path, context)?;
    context
        .declarations
        .cache()
        .knows(&krate)
        .then_some((krate, rest))
}

fn resolve_call(
    reference: &UnresolvedRef,
    context: &ExternalContext<'_>,
    names: &MethodNames,
) -> Attempt {
    let name = reference.reference_name.as_str();
    if name.contains("::") {
        return match placed(reference, context) {
            Some((krate, rest)) => path_item(reference, context, &krate, &rest),
            None => Attempt::NotOurs,
        };
    }
    if let Some((receiver, method)) = split_dotted(name) {
        if !names.may_define(method) {
            return Attempt::NotOurs;
        }
        return typed_method(context, method, ExternalResolvedBy::InstanceMethod, || {
            receiver_crate_type(receiver, reference, context)
        });
    }
    if receiver_was_dropped(reference.metadata.as_ref()) {
        if !names.may_define(name) {
            return Attempt::NotOurs;
        }
        return typed_method(context, name, ExternalResolvedBy::ReceiverChain, || {
            dropped_receiver_crate_type(reference, context)
                .or_else(|| prelude::literal_receiver(reference, context))
        });
    }
    // `f(..)`: only a name a `use` brings in from a reachable crate (or the
    // prelude's), called bare (not `self.f()`, not a local closure).
    let placed = placed(reference, context);
    if placed.is_none() && !api::is_prelude_name(context.declarations.cache(), name) {
        return Attempt::NotOurs;
    }
    match rust_call_shape(reference, context) {
        Some(RustCallShape::Bare) => match placed {
            Some((krate, rest)) => path_item(reference, context, &krate, &rest),
            None => prelude_name(reference, context),
        },
        _ => Attempt::NotOurs,
    }
}

/// The item a path (or a `use`d bare name) names in the crate it roots in.
fn path_item(
    reference: &UnresolvedRef,
    context: &ExternalContext<'_>,
    krate: &str,
    rest: &[String],
) -> Attempt {
    let declarations: &Declarations<'_> = context.declarations;
    let Some(found) = lookup_path(declarations.cache(), krate, rest, reference.reference_kind)
    else {
        return Attempt::Missed(format!("{krate}::{}", rest.join("::")));
    };
    let by = if found.reexported {
        ExternalResolvedBy::ReExport
    } else if reference.reference_name.contains("::") {
        ExternalResolvedBy::QualifiedName
    } else {
        ExternalResolvedBy::Import
    };
    Attempt::Resolved(Box::new(Resolved { found, by }))
}

/// `method` on the receiver type `infer` finds, when that type is a type of
/// a reachable crate.
fn typed_method(
    context: &ExternalContext<'_>,
    method: &str,
    by: ExternalResolvedBy,
    infer: impl FnOnce() -> Option<CrateType>,
) -> Attempt {
    let declarations: &Declarations<'_> = context.declarations;
    let before = declarations.answers();
    let inferred = infer();
    if api::tracing() {
        eprintln!(
            "{:?} typed .{method}: receiver {inferred:?}",
            std::thread::current().id()
        );
    }
    let Some(ty) = inferred.filter(|ty| declarations.cache().knows(&ty.krate)) else {
        return Attempt::NotOurs;
    };
    let through_dependency = declarations.answers() > before;
    let stand_in = stand_in(&ty);
    if stand_in == Some(StandIn::Entry) {
        return Attempt::NotOurs;
    }
    // The graph the method is found in (another crate's, when the type is
    // a re-export, an alias or derefs there).
    let found = declarations.cache().get(&ty.krate).and_then(|graph| {
        declarations.with_caller(|caller| {
            lookup_method(declarations.cache(), &graph, &ty.path, method, caller)
        })
    });
    let Some((graph, node)) = found else {
        return Attempt::Missed(format!("{}::{}::{method}", ty.krate, ty.path.join("::")));
    };
    // Any container's iterator: only a trait's own method is the one every
    // iterator runs.
    if stand_in == Some(StandIn::Iter) && !declared_by_trait(&graph, &node) {
        return Attempt::NotOurs;
    }
    Attempt::Resolved(Box::new(Resolved {
        found: Found {
            graph,
            node,
            confidence: if through_dependency { 0.85 } else { 0.9 },
            reexported: false,
        },
        by: if through_dependency {
            ExternalResolvedBy::DependencyChain
        } else {
            by
        },
    }))
}

/// Types receiver inference writes as stand-ins, not as the value's own
/// type: every container's `iter()` is `std::slice::Iter`, every map's
/// `entry(..)` a `std::collections::hash_map::Entry`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StandIn {
    Iter,
    Entry,
}

fn stand_in(ty: &CrateType) -> Option<StandIn> {
    if ty.krate != "std" {
        return None;
    }
    let path: Vec<&str> = ty.path.iter().map(String::as_str).collect();
    match path.as_slice() {
        ["slice", "Iter"] => Some(StandIn::Iter),
        ["collections", "hash_map", "Entry"] => Some(StandIn::Entry),
        _ => None,
    }
}

/// `node` is a method a trait declares (`Iterator::map`), not an impl's.
fn declared_by_trait(graph: &super::open::ForeignGraph, node: &crate::types::Node) -> bool {
    let Some((owner, _)) = node.qualified_name.rsplit_once("::") else {
        return false;
    };
    let owner = owner.rsplit("::").next().unwrap_or(owner);
    graph
        .context
        .get_nodes_in_file_named(&node.file_path, owner)
        .iter()
        .any(|candidate| {
            candidate.kind == crate::types::NodeKind::Trait
                && candidate.start_line <= node.start_line
                && candidate.end_line >= node.end_line
        })
}

/// `recv.method` (a plain or dotted receiver) as recorded for a method call.
fn split_dotted(name: &str) -> Option<(&str, &str)> {
    let (receiver, method) = name.rsplit_once('.')?;
    let ident = |text: &str| {
        !text.is_empty()
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    };
    (ident(method) && receiver.split('.').all(ident)).then_some((receiver, method))
}

#[cfg(test)]
mod tests {
    use super::split_dotted;

    #[test]
    fn splits_recorded_method_calls() {
        assert_eq!(split_dotted("node.walk"), Some(("node", "walk")));
        assert_eq!(split_dotted("a.b.walk"), Some(("a.b", "walk")));
        assert_eq!(split_dotted("walk"), None);
        assert_eq!(split_dotted("a::b"), None);
    }
}

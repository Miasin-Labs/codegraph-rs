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

pub(crate) mod lookup;
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
use crate::resolution::types::UnresolvedRef;
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
            match placed(reference, context) {
                Some((krate, rest)) => path_item(reference, context, &krate, &rest),
                None => Attempt::NotOurs,
            }
        }
        _ => Attempt::NotOurs,
    }
}

/// The reachable crate and the path inside it the reference's name
/// spells, `use` declarations followed — a cheap test most references
/// fail before anything else is read.
fn placed(
    reference: &UnresolvedRef,
    context: &ExternalContext<'_>,
) -> Option<(String, Vec<String>)> {
    let (krate, rest) = external_path(&reference.reference_name, &reference.file_path, context)?;
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
        });
    }
    // `f(..)`: only a name a `use` brings in from a reachable crate, called
    // bare (not `self.f()`, not a local closure).
    let Some((krate, rest)) = placed(reference, context) else {
        return Attempt::NotOurs;
    };
    match rust_call_shape(reference, context) {
        Some(RustCallShape::Bare) => path_item(reference, context, &krate, &rest),
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
    let Some(ty) = infer().filter(|ty| declarations.cache().knows(&ty.krate)) else {
        return Attempt::NotOurs;
    };
    let through_dependency = declarations.answers() > before;
    let found = declarations.cache().get(&ty.krate).and_then(|graph| {
        let node = lookup_method(declarations.cache(), &graph, &ty.path, method)?;
        Some((graph, node))
    });
    let Some((graph, node)) = found else {
        return Attempt::Missed(format!("{}::{}::{method}", ty.krate, ty.path.join("::")));
    };
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

//! Rust calls whose syntax decides what they can run.
//!
//! - A method call (`a.b().m()`, `self.m()`) runs a method: never a free
//!   fn, a field, or a local.
//! - A bare call (`f(..)`) runs a local closure or fn pointer, a free fn, or
//!   a tuple struct or tuple variant constructor: never a method or an
//!   associated fn, which need a receiver, `Self::`, or `Type::`.
//!
//! Extraction names `self.m()` and `m()` alike as a bare `m`, so the source
//! at the call site tells them apart. Beyond the kind of target, a bare call
//! resolves to nothing when its name is a local of the enclosing fn
//! ([`is_local_at_call`]), and it runs a std prelude value (`Ok(..)`,
//! `Some(..)`, `drop(..)`) unless the file brings a same-named project item
//! into scope. A tuple variant is callable bare only where a `use` brings it
//! into scope (`use Kind::*`, `use Kind::{Leaf}`).
//!
//! A method call on a dropped receiver resolves on the receiver's type when
//! the chain extraction recorded can be typed link by link (`self.field`,
//! `Type::new(..).build()`, `self.cache.borrow_mut()`; see
//! [`infer_rust_chain_type`]), and otherwise stays unresolved when std
//! defines a method of its name (see [`is_receiverless_std_method_call`]);
//! one a direct dependency defines reaches only a method of a type the file
//! names (see `dependency_names`).
//!
//! The same scope rules gate bare-named references ([`match_rust_reference`]):
//! an `impl`/derive/supertrait target must be a trait, and an enum variant is
//! reachable unqualified only through a `use`.

use super::dependency_names::FileText;
use super::exact::pick_exact;
use super::receiver::{
    RustType,
    file_is_module,
    fn_local_uses,
    infer_rust_chain_type,
    is_local_at_call,
    self_field_receiver_type,
};
use super::rust_method::match_typed_call;
use super::std_methods::{is_receiverless_dependency_method_call, is_receiverless_std_method_call};
use super::{UseBinding, UseLeaf};
use crate::resolution::types::{ResolutionContext, ResolvedRef, UnresolvedRef};
use crate::types::{
    EdgeKind,
    Language,
    Node,
    NodeKind,
    dropped_receiver_text,
    receiver_was_dropped,
};

/// Values the std prelude brings into every module that a bare call can
/// run: the `Option`/`Result` variants and the prelude fns.
const PRELUDE_VALUES: &[&str] = &[
    "Err",
    "None",
    "Ok",
    "Some",
    "align_of",
    "align_of_val",
    "drop",
    "size_of",
    "size_of_val",
];

/// Types the std prelude brings into every module: a bare reference to one
/// names std's unless the file defines or imports a project namesake (as
/// `crate::error::Result` is).
const PRELUDE_TYPES: &[&str] = &["Box", "Option", "Result", "String", "Vec"];

/// Crates whose `use` paths never name a project item.
const STD_CRATES: &[&str] = &["alloc", "core", "std"];

/// Decide a Rust call by its syntax: `Some(result)` is final, `None` leaves
/// the call to the other strategies.
pub(super) fn match_rust_call(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<Option<ResolvedRef>> {
    let syntax = Syntax::of(reference, context)?;
    if syntax == Syntax::DroppedReceiver {
        // No project method has the name: nothing to type the chain for.
        let named = context.get_nodes_by_name(&reference.reference_name);
        if !named
            .iter()
            .any(|node| node.language == Language::Rust && node.kind == NodeKind::Method)
        {
            return Some(None);
        }
        // `self.graph.get(x)`, `self.cache.borrow_mut().clear()`: the
        // receiver's type, followed link by link, decides as for a typed
        // local.
        let typed = dropped_receiver_type(reference, context)
            .and_then(|ty| match_typed_call(&ty, &reference.reference_name, reference, context));
        if let Some(decided) = typed {
            return Some(decided);
        }
    }
    if is_receiverless_std_method_call(reference) {
        return Some(None);
    }
    // A dependency's method name reaches only types the file names.
    let file = is_receiverless_dependency_method_call(reference, context)
        .then(|| FileText::read(reference, context));
    let mut scope = FileScope::new(reference, context);
    let candidates: Vec<Node> = context
        .get_nodes_by_name(&reference.reference_name)
        .into_iter()
        .filter(|node| {
            syntax.admits(node, &mut scope)
                && file.as_ref().is_none_or(|file| file.names_owner_of(node))
        })
        .collect();
    if candidates.is_empty() || syntax.names_local(reference, context) {
        return Some(None);
    }
    Some(pick_exact(reference, &candidates, context, None))
}

/// Whether a Rust call could run `target` given its syntax: always for a
/// call whose syntax says nothing (`recv.m`, `a::b`) or another language.
pub(crate) fn rust_call_admits(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
    target: &Node,
) -> bool {
    let Some(syntax) = Syntax::of(reference, context) else {
        return true;
    };
    !is_receiverless_std_method_call(reference)
        && (!is_receiverless_dependency_method_call(reference, context)
            || FileText::read(reference, context).names_owner_of(target))
        && syntax.admits(target, &mut FileScope::new(reference, context))
        && !syntax.names_local(reference, context)
}

/// How a Rust call spells its callee, for the external resolution pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RustCallShape {
    /// `f(..)`, naming no local of the enclosing fn.
    Bare,
    /// `f(..)` where `f` is a local (a closure, a fn pointer).
    BareLocal,
    /// `self.m(..)`.
    OnSelf,
    /// `a.b().m(..)`, recorded as `m` with the receiver text.
    DroppedReceiver,
}

/// The shape of a Rust call recorded under a plain name; `None` for any
/// other reference (a path, `recv.m`, another language).
pub(crate) fn rust_call_shape(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<RustCallShape> {
    Some(match Syntax::of(reference, context)? {
        Syntax::Bare if Syntax::Bare.names_local(reference, context) => RustCallShape::BareLocal,
        Syntax::Bare => RustCallShape::Bare,
        Syntax::OnSelf => RustCallShape::OnSelf,
        Syntax::DroppedReceiver => RustCallShape::DroppedReceiver,
    })
}

/// The type of the receiver a Rust method call dropped: its recorded text
/// followed link by link, else (no text recorded) a `self.field` read from
/// the call site.
pub(super) fn dropped_receiver_type(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<RustType> {
    dropped_receiver_text(reference.metadata.as_ref())
        .and_then(|receiver| infer_rust_chain_type(receiver, reference, context))
        .or_else(|| self_field_receiver_type(reference, context))
}

/// Decide a bare-named Rust reference whose role limits its target: an
/// `impl Trait for T`, derive or supertrait names a trait, and an enum
/// variant is visible unqualified only where a `use` brings it in (a bare
/// `String` or `Path` in a type is std's, not some `Value::String`).
/// `Some(result)` is final; `None` when no candidate is ruled out, leaving
/// the reference to the other strategies unchanged.
pub(super) fn match_rust_reference(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<Option<ResolvedRef>> {
    if !is_role_gated_reference(reference) {
        return None;
    }
    let mut scope = FileScope::new(reference, context);
    let (admitted, ruled_out): (Vec<Node>, Vec<Node>) = context
        .get_nodes_by_name(&reference.reference_name)
        .into_iter()
        .partition(|node| reference_admits(reference.reference_kind, node, &mut scope));
    if ruled_out.is_empty() {
        return None;
    }
    if admitted.is_empty() {
        return Some(None);
    }
    Some(pick_exact(reference, &admitted, context, None))
}

/// Whether a bare-named Rust reference could name `target` given its role:
/// always for other references and languages.
pub(crate) fn rust_reference_admits(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
    target: &Node,
) -> bool {
    !is_role_gated_reference(reference)
        || reference_admits(
            reference.reference_kind,
            target,
            &mut FileScope::new(reference, context),
        )
}

fn is_role_gated_reference(reference: &UnresolvedRef) -> bool {
    reference.language == Language::Rust
        && matches!(
            reference.reference_kind,
            EdgeKind::References | EdgeKind::Implements | EdgeKind::Extends
        )
        && is_identifier(&reference.reference_name)
}

fn reference_admits(kind: EdgeKind, node: &Node, scope: &mut FileScope<'_>) -> bool {
    match kind {
        EdgeKind::Implements | EdgeKind::Extends => {
            node.language == Language::Rust && node.kind == NodeKind::Trait
        }
        _ => {
            let visible = node.kind != NodeKind::EnumMember
                || (node.language == Language::Rust && scope.variant_in_scope(node));
            let prelude = PRELUDE_VALUES.contains(&node.name.as_str())
                || PRELUDE_TYPES.contains(&node.name.as_str());
            visible && (!prelude || node.kind == NodeKind::EnumMember || scope.item_in_scope(node))
        }
    }
}

/// How a call spells its callee.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Syntax {
    /// `f(..)`.
    Bare,
    /// `self.m(..)`.
    OnSelf,
    /// `a.b().m(..)`, recorded as `m`.
    DroppedReceiver,
}

impl Syntax {
    /// The syntax of a Rust call recorded under a plain name; `None` for
    /// anything else, or when the call site cannot be read.
    fn of(reference: &UnresolvedRef, context: &dyn ResolutionContext) -> Option<Syntax> {
        if reference.language != Language::Rust || reference.reference_kind != EdgeKind::Calls {
            return None;
        }
        if receiver_was_dropped(reference.metadata.as_ref()) {
            return Some(Syntax::DroppedReceiver);
        }
        let name = reference.reference_name.as_str();
        if !is_identifier(name) {
            return None;
        }
        let source = context.read_file_arc(&reference.file_path)?;
        let line = source
            .split('\n')
            .nth((reference.line as usize).checked_sub(1)?)?;
        let at = line.get(reference.column as usize..)?;
        if strip_word(at, name).is_some() {
            return Some(Syntax::Bare);
        }
        // `self.m()`, or `self` ending the line before a `.m()` below it.
        let after_self = strip_word(at, "self")?.trim_start();
        (after_self.is_empty() || after_self.starts_with('.')).then_some(Syntax::OnSelf)
    }

    /// A call spelled this way can run `node`.
    fn admits(self, node: &Node, scope: &mut FileScope<'_>) -> bool {
        match self {
            Syntax::OnSelf | Syntax::DroppedReceiver => {
                node.language == Language::Rust && node.kind == NodeKind::Method
            }
            Syntax::Bare => {
                let callable = match node.kind {
                    // An `extern "C"` fn may be indexed in its own language.
                    NodeKind::Function => true,
                    // A fn item nested in a method body is indexed as a
                    // method of the impl; it is callable bare where its
                    // enclosing fn's body is.
                    NodeKind::Method => {
                        node.language == Language::Rust && scope.nested_fn_in_scope(node)
                    }
                    NodeKind::Struct | NodeKind::Variable | NodeKind::Constant => {
                        node.language == Language::Rust
                    }
                    NodeKind::EnumMember => {
                        node.language == Language::Rust && scope.variant_in_scope(node)
                    }
                    _ => false,
                };
                callable
                    && (!PRELUDE_VALUES.contains(&node.name.as_str())
                        || node.kind == NodeKind::EnumMember
                        || scope.item_in_scope(node))
            }
        }
    }

    /// A bare call naming a local of the enclosing fn.
    fn names_local(self, reference: &UnresolvedRef, context: &dyn ResolutionContext) -> bool {
        self == Syntax::Bare && is_local_at_call(&reference.reference_name, reference, context)
    }
}

/// What is in scope at the call site: the referencing file's `use`
/// declarations and fns, each read on first need.
struct FileScope<'a> {
    reference: &'a UnresolvedRef,
    context: &'a dyn ResolutionContext,
    leaves: Option<Vec<UseLeaf>>,
    /// The Rust fns and methods of the referencing file, as line ranges.
    fns: Option<Vec<(String, u32, u32)>>,
}

impl<'a> FileScope<'a> {
    fn new(reference: &'a UnresolvedRef, context: &'a dyn ResolutionContext) -> Self {
        FileScope {
            reference,
            context,
            leaves: None,
            fns: None,
        }
    }

    /// `node` is a fn item nested in the body of a fn that also holds the
    /// call (`fn resolve() { fn helper() {} … helper() }`), which the index
    /// records as a method when the outer fn is one.
    fn nested_fn_in_scope(&mut self, node: &Node) -> bool {
        if node.file_path != self.reference.file_path {
            return false;
        }
        let line = self.reference.line;
        let fns = self.fns.get_or_insert_with(|| {
            self.context
                .get_nodes_in_file(&self.reference.file_path)
                .into_iter()
                .filter(|node| {
                    node.language == Language::Rust
                        && matches!(node.kind, NodeKind::Function | NodeKind::Method)
                })
                .map(|node| (node.id, node.start_line, node.end_line.max(node.start_line)))
                .collect()
        });
        fns.iter().any(|(id, start, end)| {
            *id != node.id
                && *start < node.start_line
                && node.end_line <= *end
                && (*start..=*end).contains(&line)
        })
    }

    /// The file's `use` leaves not rooted in a std crate, those inside fn
    /// bodies included.
    fn leaves(&mut self) -> &[UseLeaf] {
        self.leaves.get_or_insert_with(|| {
            let file = &self.reference.file_path;
            let mut leaves: Vec<UseLeaf> = self
                .context
                .get_rust_use_leaves(file)
                .iter()
                .map(|found| found.leaf.clone())
                .collect();
            leaves.extend(fn_local_uses(file, self.context).iter().cloned());
            leaves.retain(|leaf| {
                leaf.path
                    .first()
                    .is_none_or(|root| !STD_CRATES.contains(&root.as_str()))
            });
            leaves
        })
    }

    /// The tuple variant `node` (`Kind::Leaf`) is in scope unqualified:
    /// `use …Kind::*` or `use …Kind::Leaf`.
    fn variant_in_scope(&mut self, node: &Node) -> bool {
        let Some((enum_path, variant)) = node.qualified_name.rsplit_once("::") else {
            return false;
        };
        let enum_name = enum_path.rsplit("::").next().unwrap_or(enum_path);
        self.leaves().iter().any(|leaf| {
            let mut path = leaf.path.iter().rev();
            match &leaf.binding {
                UseBinding::Glob => path.next().is_some_and(|last| last == enum_name),
                UseBinding::Name(bound) => {
                    bound == variant
                        && path.next().is_some_and(|last| last == variant)
                        && path.next().is_some_and(|owner| owner == enum_name)
                }
                UseBinding::Module(_) => false,
            }
        })
    }

    /// The fn or struct `node` is defined in the referencing file, or a `use`
    /// names it or glob-imports its module.
    fn item_in_scope(&mut self, node: &Node) -> bool {
        if node.file_path == self.reference.file_path {
            return true;
        }
        self.leaves().iter().any(|leaf| match &leaf.binding {
            UseBinding::Name(bound) => {
                *bound == node.name && leaf.path.last().is_some_and(|last| *last == node.name)
            }
            UseBinding::Glob => leaf
                .path
                .last()
                .is_some_and(|module| file_is_module(&node.file_path, module)),
            UseBinding::Module(_) => false,
        })
    }
}

/// `text` starts with the identifier `word`: the rest after it.
fn strip_word<'t>(text: &'t str, word: &str) -> Option<&'t str> {
    let rest = text.strip_prefix(word)?;
    let continues = rest
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    (!continues).then_some(rest)
}

fn is_identifier(name: &str) -> bool {
    name.bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

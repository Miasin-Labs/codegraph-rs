//! Rust method calls: resolved on the receiver's type, never guessed.
//!
//! After receiver-type inference, `match_method_call` falls back to
//! strategies that pick a same-named method on some other type: a class named
//! like the receiver, the project's only method of that name, or the one
//! whose owner shares a word with the receiver. For Rust those guesses are
//! wrong whenever the call runs a std method (`chars.next()` landing on
//! `FrontierIter::next`, `HashMap::new()` on `OrderedNodeMap::new`):
//!
//! - `recv.m(..)` resolves on the type [`infer_rust_receiver_type`] finds.
//!   A type the project does not define (`Vec`, `tree_sitter::Node`) runs no
//!   project method, and a common std method name (`next`, `get`, `iter`)
//!   is not guessed at when no project method was found. When no type is
//!   found, no method name std defines anywhere (`into_inner`, `parent`) is
//!   guessed at either, and one a direct dependency defines (tree-sitter's
//!   `walk`) only onto a type the calling file names
//!   ([`match_dependency_method`]).
//! - `Type::m(..)` names its type, so the target is `Type::m` or nothing: a
//!   type the project does not define (`HashMap`, `process`, a generic `T`)
//!   resolves to nothing, and a project type resolves on itself, after type
//!   aliases and `use … as` renames.
//!
//! Only a project-specific method name a project type does not define itself
//! (a trait's provided method, a `Deref` target's method) still reaches the
//! fallbacks.

use serde_json::Value;

use super::dependency_names::match_dependency_method;
use super::exact::pick_exact;
use super::receiver::{Dispatch, RustType, generic_param, infer_rust_receiver_type, resolve_type};
use super::rust_call::TYPED_CONFIDENCE;
use super::rust_generics::is_generic_param;
use super::std_methods::{is_common_std_method_name, is_std_method_name};
use crate::resolution::types::{ResolutionContext, ResolvedBy, ResolvedRef, UnresolvedRef};
use crate::types::Node;

/// Decide `receiver.method(..)`: `Some(result)` is final, `None` leaves the
/// call to the remaining strategies.
pub(super) fn match_instance_call(
    receiver: &str,
    method: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<Option<ResolvedRef>> {
    let Some(ty) = infer_rust_receiver_type(receiver, reference, context) else {
        if is_std_method_name(method) {
            return Some(None);
        }
        return match_dependency_method(method, reference, context);
    };
    decide_on_type(
        &ty,
        method,
        reference,
        context,
        TYPED_CONFIDENCE,
        ResolvedBy::InstanceMethod,
    )
}

/// Decide a call of `method` on a receiver of the known type `ty` (a
/// `self.field` receiver extraction dropped) like [`match_instance_call`].
pub(super) fn match_typed_call(
    ty: &RustType,
    method: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<Option<ResolvedRef>> {
    decide_on_type(
        ty,
        method,
        reference,
        context,
        TYPED_CONFIDENCE,
        ResolvedBy::InstanceMethod,
    )
}

/// Decide `owner::method(..)` like [`match_instance_call`].
pub(super) fn match_type_path_call(
    owner: &str,
    method: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<Option<ResolvedRef>> {
    if owner == "Self" {
        // `match_rust_path` already looked in the enclosing impl.
        return is_common_std_method_name(method).then_some(None);
    }
    // `T::default()`, `R::new()`: a generic parameter's, not a project
    // type's that shares its name — what its project-trait bounds declare
    // (`L::pointers(..)` with `L: Link` runs `Link::pointers`), else nothing.
    if let Some(ty) = generic_param(owner, &reference.file_path, reference, context) {
        return Some(
            decide_on_type(
                &ty,
                method,
                reference,
                context,
                0.85,
                ResolvedBy::QualifiedName,
            )
            .flatten(),
        );
    }
    if is_generic_param(owner, reference, context) {
        return Some(None);
    }
    let ty = resolve_type(owner, &reference.file_path, reference, context);
    if !ty.is_project_type(context) {
        return Some(None);
    }
    decide_on_type(
        &ty,
        method,
        reference,
        context,
        0.85,
        ResolvedBy::QualifiedName,
    )
}

/// Decide `Type::m(..)` whose `Type` a `use` or path places in another
/// crate (`use std::sync::atomic::AtomicUsize; AtomicUsize::new(0)`): the
/// project runs it only through an impl on that foreign type, never a
/// same-named project type's (tokio's own `loom::std::AtomicUsize::new`).
/// `None` for any other path, which the qualified-name rules keep.
pub(super) fn match_foreign_type_path(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<Option<ResolvedRef>> {
    let (owner, method) = reference.reference_name.rsplit_once("::")?;
    if !is_path(owner) || !is_path(method) {
        return None;
    }
    let ty = resolve_type(owner, &reference.file_path, reference, context);
    ty.external_crate()?;
    Some(
        decide_on_type(
            &ty,
            method,
            reference,
            context,
            0.85,
            ResolvedBy::QualifiedName,
        )
        .flatten(),
    )
}

/// `a::b::C` written plainly: identifiers only, no generics or turbofish.
fn is_path(text: &str) -> bool {
    text.split("::").all(|segment| {
        segment
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    })
}

fn decide_on_type(
    ty: &RustType,
    method: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
    confidence: f64,
    resolved_by: ResolvedBy,
) -> Option<Option<ResolvedRef>> {
    let candidates = context.get_nodes_by_name(method);
    let methods = ty.methods(method, &candidates, reference, context);
    if let Some(dispatch) = ty.dispatch() {
        return Some(dispatched_call(&methods, dispatch, reference, confidence));
    }
    let target = match methods.as_slice() {
        [] => None,
        [only] => Some(only.id.clone()),
        // Tied on the type's home: the name rules' nearness picks, never
        // the calling fn itself — `Type::m(self)` or `self.m()` inside a
        // trait impl's `m` runs the inherent `m` it wraps.
        tied => {
            let tied: Vec<Node> = tied
                .iter()
                .filter(|node| node.id != reference.from_node_id)
                .map(|node| (*node).clone())
                .collect();
            pick_exact(reference, &tied, context, None).map(|picked| picked.target_node_id)
        }
    };
    if let Some(target_node_id) = target {
        return Some(Some(ResolvedRef {
            original: reference.clone(),
            target_node_id,
            confidence,
            resolved_by,
        }));
    }
    if !ty.is_project_type(context) || is_common_std_method_name(method) {
        return Some(None);
    }
    None
}

/// A call on a trait object or a bounded generic runs whichever
/// implementation the value has; its one static target is the trait's
/// declaration (the implementations hang off it as dispatch edges), and
/// two traits declaring the name are no target at all. The edge says how
/// it dispatches (`dispatch`: `dynamic` or `generic`).
fn dispatched_call(
    declared: &[&Node],
    dispatch: Dispatch,
    reference: &UnresolvedRef,
    confidence: f64,
) -> Option<ResolvedRef> {
    let [declaration] = declared else {
        return None;
    };
    let mut original = reference.clone();
    original
        .metadata
        .get_or_insert_with(Default::default)
        .insert("dispatch".to_string(), Value::from(dispatch.as_str()));
    Some(ResolvedRef {
        original,
        target_node_id: declaration.id.clone(),
        confidence,
        resolved_by: ResolvedBy::TraitDispatch,
    })
}

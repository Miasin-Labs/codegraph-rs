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
//!   is not guessed at when no project method was found.
//! - `Type::m(..)` names its type, so the target is `Type::m` or nothing: a
//!   type the project does not define (`HashMap`, `process`, a generic `T`)
//!   resolves to nothing, and a project type resolves on itself, after type
//!   aliases and `use … as` renames.
//!
//! Only a project-specific method name a project type does not define itself
//! (a trait's provided method, a `Deref` target's method) still reaches the
//! fallbacks.

use super::receiver::{RustType, infer_rust_receiver_type, resolve_type};
use super::std_methods::is_common_std_method_name;
use crate::resolution::types::{ResolutionContext, ResolvedBy, ResolvedRef, UnresolvedRef};

/// Decide `receiver.method(..)`: `Some(result)` is final, `None` leaves the
/// call to the remaining strategies.
pub(super) fn match_instance_call(
    receiver: &str,
    method: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<Option<ResolvedRef>> {
    let Some(ty) = infer_rust_receiver_type(receiver, reference, context) else {
        return is_common_std_method_name(method).then_some(None);
    };
    decide_on_type(
        &ty,
        method,
        reference,
        context,
        0.9,
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

fn decide_on_type(
    ty: &RustType,
    method: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
    confidence: f64,
    resolved_by: ResolvedBy,
) -> Option<Option<ResolvedRef>> {
    let candidates = context.get_nodes_by_name(method);
    if let Some(target) = ty.method(method, &candidates, reference, context) {
        return Some(Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: target.id.clone(),
            confidence,
            resolved_by,
        }));
    }
    if !ty.is_project_type(context) || is_common_std_method_name(method) {
        return Some(None);
    }
    None
}

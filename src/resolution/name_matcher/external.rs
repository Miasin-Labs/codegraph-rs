//! What the external resolution pass ([`crate::resolution::external`])
//! asks of Rust name matching: how a call is spelled, what type its
//! receiver has, and which crate outside the project a path names.
//!
//! The answers come from the same inference in-project resolution uses; run
//! with a context that answers [`ResolutionContext::foreign_types`], a
//! receiver's chain is typed through the dependencies' declared return
//! types as well.

use super::receiver::{RustType, infer_rust_chain_type, infer_rust_receiver_type};
pub(crate) use super::rust_call::{RustCallShape, rust_call_shape};
use crate::resolution::types::{ResolutionContext, UnresolvedRef};

/// A type of a crate outside the project: the crate as code names it, and
/// the type's path inside it (`("rusqlite", ["Statement"])`, `("regex",
/// ["bytes", "Regex"])`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CrateType {
    pub(crate) krate: String,
    pub(crate) path: Vec<String>,
}

impl CrateType {
    fn of(ty: RustType, context: &dyn ResolutionContext) -> Option<CrateType> {
        if let Some(krate) = ty.external_crate() {
            return Some(CrateType {
                krate: krate.to_string(),
                path: ty.external_path().to_vec(),
            });
        }
        // `Vec`, `String`, `Option`… named bare: the toolchain's.
        let (krate, path) = ty.prelude_home(context)?;
        Some(CrateType {
            krate: krate.to_string(),
            path: path.iter().map(|segment| segment.to_string()).collect(),
        })
    }
}

/// The module path of a Rust file inside its crate (`src/a/b.rs` →
/// `["a", "b"]`; a crate root → `[]`).
pub(crate) fn module_path(file_path: &str) -> Vec<String> {
    super::rust_path::module_of(file_path)
}

/// The type of the receiver `receiver` of `receiver.method(..)` (a local,
/// or a dotted chain of fields) when it is a type of a crate outside the
/// project.
pub(crate) fn receiver_crate_type(
    receiver: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<CrateType> {
    let ty = if receiver.contains('.') {
        infer_rust_chain_type(receiver, reference, context)
    } else {
        infer_rust_receiver_type(receiver, reference, context)
    };
    CrateType::of(ty?, context)
}

/// The type of the receiver a method call dropped (`a.b().m()` recorded as
/// `m`) when it is a type of a crate outside the project.
pub(crate) fn dropped_receiver_crate_type(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<CrateType> {
    CrateType::of(
        super::rust_call::dropped_receiver_type(reference, context)?,
        context,
    )
}

/// The return type written in a Rust fn signature (`None` for `()`).
pub(crate) fn signature_return(signature: &str) -> Option<&str> {
    super::receiver::signature_return(signature)
}

/// The type written in a Rust field's declaration, read from its source.
pub(crate) fn field_declared_type(
    field: &crate::types::Node,
    context: &dyn ResolutionContext,
) -> Option<String> {
    super::receiver::declared_type(field, context)
}

/// The crate outside the project and the path inside it that `path`,
/// written in `file`, names (see `receiver::rust::lookup::external_path`).
pub(crate) fn external_path(
    path: &str,
    file: &str,
    context: &dyn ResolutionContext,
) -> Option<(String, Vec<String>)> {
    let (root, rest) = super::receiver::external_path(path, file, context)?;
    // `Vec::with_capacity`: a prelude type named bare is the toolchain's.
    if let Some((krate, module, name)) = super::receiver::prelude_type(&root, context) {
        let mut full = vec![module.to_string(), name.to_string()];
        full.extend(rest);
        return Some((krate.to_string(), full));
    }
    Some((root, rest))
}

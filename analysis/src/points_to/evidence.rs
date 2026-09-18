//! What a call op's qualifier says about a same-named target.
//!
//! `O` is the target's owner (the second-to-last segment of its qualified
//! name), `C` the caller's:
//!
//! | callee text | evidence for a target `T` named `f` |
//! |---|---|
//! | `f` | `O == C` (a sibling nested function, or the caller recursing) confirms; a method or associated function is ruled out — every IR language reaches those only through a path, a receiver or the type |
//! | `Q::f` | `O == Q`, or a free `f` in a module/file named `Q`, confirms; any other `O` rules `T` out |
//! | `Self::f`, `self.f`, `this.f` | `O == C` confirms |
//! | `crate::f`, `super::f`, `<T as Tr>::f` | none |
//! | `q.f` | `O == q` (a class-qualified call), or a free `f` in a module named `q`, confirms; otherwise none — `q` is a value of unknown type |
//! | `self.x.f`, `this.x.f` | as `q.f`, but `O == C` rules `T` out: a field rarely has its owner's type, and delegation (`fn len(&self) { self.inner.len() }`) is exactly where name-only resolution invents such edges |

use super::call_text::{CalleeText, Qualifier};
use super::target::Target;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Evidence {
    Against,
    None,
    For,
}

/// How strongly `text`, written in a function owned by `caller_owner`,
/// names `target` (see the module docs).
pub(super) fn evidence(
    text: &CalleeText<'_>,
    target: &Target<'_>,
    caller_owner: Option<&str>,
) -> Evidence {
    let same_owner = target.owner.is_some() && target.owner == caller_owner;
    if text.on_self_field && same_owner {
        return Evidence::Against;
    }
    let own_owner = if same_owner {
        Evidence::For
    } else {
        Evidence::None
    };
    match text.qualifier {
        Qualifier::Bare if target.type_owned => Evidence::Against,
        Qualifier::Bare => own_owner,
        Qualifier::Path("" | "crate" | "super" | "self") => Evidence::None,
        Qualifier::Path("Self") | Qualifier::Member("self" | "this") => own_owner,
        Qualifier::Path(seg) => match target.owner {
            Some(owner) if owner == seg => Evidence::For,
            None if target.lives_in_module(seg) => Evidence::For,
            _ => Evidence::Against,
        },
        Qualifier::Member("") => Evidence::None,
        Qualifier::Member(seg) => match target.owner {
            Some(owner) if owner == seg => Evidence::For,
            None if target.lives_in_module(seg) => Evidence::For,
            _ => Evidence::None,
        },
    }
}

/// Is a call op with this qualifier, bound to `target`, an instance call
/// whose receiver operand is the callee's receiver? `q.f` is one unless
/// `q` names `f`'s owner (`Base.__init__(self, x)`, Go `T.M(s, x)`).
pub(super) fn is_instance_call(qualifier: Qualifier<'_>, target: &Target<'_>) -> bool {
    match qualifier {
        Qualifier::Member(seg) => target.owner != Some(seg),
        Qualifier::Bare | Qualifier::Path(_) => false,
    }
}

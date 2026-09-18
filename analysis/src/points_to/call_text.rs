//! Reading a call op's callee text: the called name and what qualifies it.
//!
//! IR lowering keeps the callee expression as source text — `f`,
//! `helper::<T>`, `Foo::new`, `crate::a::f`, `Self::g`, `self.items.push`,
//! `this.run`, `pkg.Func`, `obj?.m`. Matching a call op to the function a
//! `Calls` edge names needs the last segment (the name) and the segment
//! before it (the qualifier), with generic arguments stripped.

/// How the callee name is qualified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Qualifier<'a> {
    /// A bare name: `f(x)`.
    Bare,
    /// A path: `A::f(x)`, `crate::a::f(x)`, `Self::f(x)`. Holds the path's
    /// last segment (`A`, `a`, `Self`); empty for a qualified-self path
    /// like `<T as Trait>::f`.
    Path(&'a str),
    /// A member access: `obj.f(x)`, `self.x.f(x)`, `obj?.f(x)`. Holds the
    /// receiver expression's last segment (`obj`, `x`); empty when the
    /// receiver is not a name (`make().f(x)`).
    Member(&'a str),
}

/// A callee text split into its name and qualifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CalleeText<'a> {
    pub(super) name: &'a str,
    pub(super) qualifier: Qualifier<'a>,
    /// The receiver is a field reached through `self` / `this`
    /// (`self.inner.m`, `this.a.b.m`), not `self` itself.
    pub(super) on_self_field: bool,
}

impl<'a> CalleeText<'a> {
    /// Split `text`; `None` when it does not end in a name (`(f)(x)`,
    /// `handlers[k](x)`, `<iter::next>`).
    pub(super) fn parse(text: &'a str) -> Option<Self> {
        let text = strip_generic_args(text);
        let (rest, name) = split_trailing_ident(text);
        if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
            return None;
        }
        let mut on_self_field = false;
        let qualifier = if rest.is_empty() {
            Qualifier::Bare
        } else if let Some(prefix) = rest.strip_suffix("::") {
            Qualifier::Path(last_segment(prefix))
        } else if let Some(prefix) = rest.strip_suffix("?.").or_else(|| rest.strip_suffix('.')) {
            on_self_field = ["self.", "self?.", "this.", "this?."]
                .iter()
                .any(|root| prefix.trim_start().starts_with(root));
            Qualifier::Member(last_segment(prefix))
        } else {
            return None;
        };
        Some(Self {
            name,
            qualifier,
            on_self_field,
        })
    }
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// `(prefix, trailing identifier)`.
fn split_trailing_ident(text: &str) -> (&str, &str) {
    let start = text
        .char_indices()
        .rev()
        .take_while(|&(_, c)| is_ident_char(c))
        .last()
        .map_or(text.len(), |(idx, _)| idx);
    text.split_at(start)
}

/// The trailing identifier of a qualifier expression, generics stripped:
/// `Foo::<T>` → `Foo`, `self.items` → `items`, `make()` → ``.
fn last_segment(prefix: &str) -> &str {
    split_trailing_ident(strip_generic_args(prefix.trim_end())).1
}

/// Drop one trailing generic argument list and the turbofish `::` before
/// it: `helper::<T>` → `helper`, `Vec<u8>` → `Vec`. Text that does not end
/// in `>`, or whose `<` is unbalanced, is returned unchanged.
fn strip_generic_args(text: &str) -> &str {
    if !text.ends_with('>') {
        return text;
    }
    let mut depth = 0usize;
    for (idx, c) in text.char_indices().rev() {
        match c {
            '>' => depth += 1,
            '<' => {
                depth -= 1;
                if depth == 0 {
                    let head = &text[..idx];
                    return head.strip_suffix("::").unwrap_or(head).trim_end();
                }
            }
            _ => {}
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{CalleeText, Qualifier};

    fn parse(text: &str) -> Option<(&str, Qualifier<'_>)> {
        CalleeText::parse(text).map(|c| (c.name, c.qualifier))
    }

    #[test]
    fn splits_name_and_qualifier() {
        assert_eq!(parse("f"), Some(("f", Qualifier::Bare)));
        assert_eq!(parse("helper::<T>"), Some(("helper", Qualifier::Bare)));
        assert_eq!(parse("Foo::new"), Some(("new", Qualifier::Path("Foo"))));
        assert_eq!(
            parse("Foo::<T>::new"),
            Some(("new", Qualifier::Path("Foo")))
        );
        assert_eq!(parse("crate::a::f"), Some(("f", Qualifier::Path("a"))));
        assert_eq!(parse("Self::g"), Some(("g", Qualifier::Path("Self"))));
        assert_eq!(parse("<T as Tr>::m"), Some(("m", Qualifier::Path(""))));
        assert_eq!(parse("self.m"), Some(("m", Qualifier::Member("self"))));
        assert_eq!(parse("self.x.m::<u8>"), Some(("m", Qualifier::Member("x"))));
        assert_eq!(parse("this.run"), Some(("run", Qualifier::Member("this"))));
        assert_eq!(parse("pkg.Func"), Some(("Func", Qualifier::Member("pkg"))));
        assert_eq!(parse("obj?.m"), Some(("m", Qualifier::Member("obj"))));
        assert_eq!(parse("make().m"), Some(("m", Qualifier::Member(""))));
        assert_eq!(
            parse("Vec::<Vec<u8>>::new"),
            Some(("new", Qualifier::Path("Vec")))
        );
    }

    #[test]
    fn marks_receivers_reached_through_self_fields() {
        let on_self_field = |text| CalleeText::parse(text).unwrap().on_self_field;
        assert!(on_self_field("self.inner.len"));
        assert!(on_self_field("this.a.b.run"));
        assert!(!on_self_field("self.len"));
        assert!(!on_self_field("this.run"));
        assert!(!on_self_field("other.inner.len"));
        assert!(!on_self_field("Self::new"));
    }

    #[test]
    fn rejects_callees_without_a_trailing_name() {
        for text in ["", "(f)", "handlers[k]", "<iter::next>", "t.0", "*f"] {
            assert_eq!(parse(text), None, "{text:?}");
        }
    }
}

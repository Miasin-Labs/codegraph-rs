//! Rust initializer expressions, read far enough to know where their type
//! comes from.
//!
//! An initializer is one head expression followed by postfix operations.
//! The head either spells a type (`Foo { .. }`, `Foo(..)`, `"text"`), calls
//! something whose signature does (`Foo::new(..)`, `build(..)`), or copies
//! another local. Of the postfix operations only those with a known effect
//! on the type are followed (`?`, `.unwrap()`, `.clone()`, `.await`, ...);
//! anything else (`.iter()`, `+ 1`) makes the type unknown.

use super::types::{
    SLICE,
    is_ident,
    is_ident_byte,
    matching_close,
    matching_paren,
    starts_uppercase,
};

/// Where an initializer's type comes from.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Head<'a> {
    /// A type the expression spells (`Self` included).
    Named(String),
    /// A call of `path`; its type is the callee's return type.
    Call { path: Vec<&'a str>, args: &'a str },
    /// Another local, moved, copied, or borrowed.
    Local(&'a str),
}

/// A postfix operation that changes the type in a known way.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Tail {
    /// `?`, `.unwrap()`, `.expect(..)`: `Result<T, _>`/`Option<T>` to `T`.
    Unwrap,
    /// `.clone()`, `.to_owned()`, `.await`: the type is kept.
    Same,
    /// `.to_string()`, `.collect::<T>()`, `as T`: the type is spelled.
    Spelled(String),
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Initializer<'a> {
    pub(super) head: Head<'a>,
    pub(super) tails: Vec<Tail>,
}

const KEYWORDS: &[&str] = &[
    "async", "break", "continue", "if", "loop", "match", "move", "return", "unsafe", "while",
];

/// The byte offset of the `;` ending the statement that `text` starts, if
/// it ends inside `text`.
pub(super) fn statement_end(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => index = matching_string(text, index)?,
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.checked_sub(1)?,
            b';' if depth == 0 => return Some(index),
            _ => {}
        }
        index += 1;
    }
    None
}

/// The byte offset where the scrutinee starting `text` ends in
/// `if let P = scrutinee {`, `while let …`, or `let P = scrutinee else {`.
pub(super) fn scrutinee_end(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => match matching_string(text, index) {
                Some(close) => index = close,
                None => return text.len(),
            },
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth = depth.saturating_sub(1),
            b'{' | b';' if depth == 0 => return index,
            b'e' if depth == 0
                && text[index..].starts_with("else")
                && !text[index + 4..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
                && (index == 0 || !is_ident_byte(bytes[index - 1])) =>
            {
                return index;
            }
            _ => {}
        }
        index += 1;
    }
    text.len()
}

/// The index of the quote closing the string literal opening at `start`.
fn matching_string(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 1,
            b'"' => return Some(index),
            _ => {}
        }
        index += 1;
    }
    None
}

/// Read `text` (one expression, without the trailing `;`).
pub(super) fn parse_initializer(text: &str) -> Option<Initializer<'_>> {
    let text = text.trim();
    let text = text
        .strip_prefix("&mut ")
        .or_else(|| text.strip_prefix('&'))
        .unwrap_or(text);
    let (head, rest) = head(text.trim_start())?;
    Some(Initializer {
        head,
        tails: tails(rest)?,
    })
}

fn named(name: &str) -> Head<'static> {
    Head::Named(name.to_string())
}

fn head(text: &str) -> Option<(Head<'_>, &str)> {
    let first = *text.as_bytes().first()?;
    match first {
        b'"' => {
            let close = matching_string(text, 0)?;
            Some((named("str"), &text[close + 1..]))
        }
        b'[' => {
            let close = matching_paren(text)?;
            Some((named(SLICE), &text[close + 1..]))
        }
        b'0'..=b'9' => {
            let digits = |from: usize| {
                text[from..]
                    .bytes()
                    .position(|byte| !is_ident_byte(byte))
                    .map_or(text.len(), |length| from + length)
            };
            let end = digits(0);
            let fraction = text[end..]
                .strip_prefix('.')
                .is_some_and(|after| after.starts_with(|c: char| c.is_ascii_digit()));
            let (number, end) = if fraction {
                ("f64", digits(end + 1))
            } else {
                ("i32", end)
            };
            Some((named(number), &text[end..]))
        }
        _ if is_ident_byte(first) => path_head(text),
        _ => None,
    }
}

/// A head starting with a path: a call, a macro, a struct literal, or a
/// path value.
fn path_head(text: &str) -> Option<(Head<'_>, &str)> {
    let (path, rest) = path(text)?;
    let last = *path.last()?;
    if path.len() == 1 && KEYWORDS.contains(&last) {
        return None;
    }
    if matches!(last, "true" | "false") && path.len() == 1 {
        return Some((named("bool"), rest));
    }
    let after = rest.trim_start();
    if let Some(macro_args) = after.strip_prefix('!') {
        let close = matching_paren(macro_args)?;
        let spelled = match last {
            "vec" => "Vec",
            "format" => "String",
            _ => return None,
        };
        return Some((named(spelled), &macro_args[close + 1..]));
    }
    if after.starts_with('(') {
        let close = matching_paren(after)?;
        let args = &after[1..close];
        return Some((Head::Call { path, args }, &after[close + 1..]));
    }
    if after.starts_with('{') && starts_uppercase(last) {
        let close = matching_paren(after)?;
        return Some((named(literal_type(&path)?), &after[close + 1..]));
    }
    Some((path_value(&path)?, rest))
}

/// The type a struct literal or enum variant path names: `Foo { .. }` and
/// `m::Foo { .. }` are `Foo`, `Foo::Variant { .. }` is `Foo`.
fn literal_type<'a>(path: &[&'a str]) -> Option<&'a str> {
    let last = *path.last()?;
    match path.len().checked_sub(2).map(|index| path[index]) {
        Some(owner) if starts_uppercase(owner) => Some(owner),
        _ => Some(last),
    }
}

/// A path used as a value: a local, a unit struct, or a unit variant.
fn path_value<'a>(path: &[&'a str]) -> Option<Head<'a>> {
    let last = *path.last()?;
    if !starts_uppercase(last) {
        return (path.len() == 1).then_some(Head::Local(last));
    }
    if last.len() > 1 && !last.bytes().any(|byte| byte.is_ascii_lowercase()) {
        // `CONST` or `Type::CONST`: the constant's type is not written here.
        return None;
    }
    if path == ["None"] {
        return Some(named("Option"));
    }
    literal_type(path).map(named)
}

/// Split a leading path (`a::B::<T>::c`) into its segments, dropping
/// turbofish generic arguments, and return what follows it.
fn path(text: &str) -> Option<(Vec<&str>, &str)> {
    let mut segments = Vec::new();
    let mut rest = text;
    loop {
        let end = rest
            .bytes()
            .position(|byte| !is_ident_byte(byte))
            .unwrap_or(rest.len());
        let segment = &rest[..end];
        if !is_ident(segment) {
            return None;
        }
        segments.push(segment);
        rest = &rest[end..];
        let Some(after) = rest.strip_prefix("::") else {
            return Some((segments, rest));
        };
        rest = after;
        if rest.starts_with('<') {
            let close = matching_close(rest)?;
            rest = &rest[close + 1..];
            match rest.strip_prefix("::") {
                Some(after) => rest = after,
                None => return Some((segments, rest)),
            }
        }
    }
}

/// The postfix operations after the head, or `None` when one of them has
/// an effect on the type this module does not follow.
fn tails(mut rest: &str) -> Option<Vec<Tail>> {
    let mut tails = Vec::new();
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            return Some(tails);
        }
        if let Some(after) = rest.strip_prefix('?') {
            tails.push(Tail::Unwrap);
            rest = after;
            continue;
        }
        if let Some(after) = rest.strip_prefix("as ") {
            tails.push(Tail::Spelled(after.trim().to_string()));
            return Some(tails);
        }
        let after = rest.strip_prefix('.')?.trim_start();
        if let Some(after) = after.strip_prefix("await") {
            if !after.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
                tails.push(Tail::Same);
                rest = after;
                continue;
            }
        }
        let (method, turbofish, after) = method_call(after)?;
        tails.push(match method {
            "unwrap" | "expect" | "unwrap_or_default" | "unwrap_or_else" | "unwrap_or" => {
                Tail::Unwrap
            }
            "clone" | "to_owned" => Tail::Same,
            "to_string" => Tail::Spelled("String".into()),
            "collect" => Tail::Spelled(turbofish?.to_string()),
            _ => return None,
        });
        rest = after;
    }
}

/// `name[::<T>](args)` -> (`name`, `T`, what follows the call).
fn method_call(text: &str) -> Option<(&str, Option<&str>, &str)> {
    let end = text
        .bytes()
        .position(|byte| !is_ident_byte(byte))
        .unwrap_or(text.len());
    let name = &text[..end];
    let mut rest = &text[end..];
    let mut turbofish = None;
    if let Some(generics) = rest.strip_prefix("::") {
        let close = matching_close(generics)?;
        turbofish = Some(&generics[1..close]);
        rest = &generics[close + 1..];
    }
    if !rest.starts_with('(') {
        return None;
    }
    let close = matching_paren(rest)?;
    Some((name, turbofish, &rest[close + 1..]))
}

#[cfg(test)]
mod tests {
    use super::{Head, Tail, parse_initializer, scrutinee_end, statement_end};

    fn head(text: &str) -> Option<Head<'_>> {
        parse_initializer(text).map(|init| init.head)
    }

    #[test]
    fn reads_heads() {
        assert_eq!(
            head("Graph::new(a, (b, c))"),
            Some(Head::Call {
                path: vec!["Graph", "new"],
                args: "a, (b, c)"
            })
        );
        assert_eq!(
            head("Vec::<u8>::with_capacity(4)"),
            Some(Head::Call {
                path: vec!["Vec", "with_capacity"],
                args: "4"
            })
        );
        assert_eq!(
            head("Config {\n a: 1,\n}"),
            Some(Head::Named("Config".into()))
        );
        assert_eq!(head("Kind::Leaf { id }"), Some(Head::Named("Kind".into())));
        assert_eq!(head("&mut other"), Some(Head::Local("other")));
        assert_eq!(head("vec![1, 2]"), Some(Head::Named("Vec".into())));
        assert_eq!(head("\"text\""), Some(Head::Named("str".into())));
        assert_eq!(head("MAX_DEPTH"), None);
        assert_eq!(head("match x { _ => 1 }"), None);
        assert_eq!(head("a + b"), None);
    }

    #[test]
    fn reads_tails() {
        let init = parse_initializer("Store::open(path)?.clone()").unwrap();
        assert_eq!(init.tails, [Tail::Unwrap, Tail::Same]);
        let init = parse_initializer("load().await.expect(\"loaded\")").unwrap();
        assert_eq!(init.tails, [Tail::Same, Tail::Unwrap]);
        let init = parse_initializer("names.iter().collect::<Vec<_>>()");
        assert_eq!(init, None);
        let init = parse_initializer("raw.collect::<HashSet<u32>>()").unwrap();
        assert_eq!(init.tails, [Tail::Spelled("HashSet<u32>".into())]);
    }

    #[test]
    fn finds_the_scrutinee_end() {
        let text = " find(id, |x| { x }) else {";
        assert_eq!(&text[..scrutinee_end(text)], " find(id, |x| { x }) ");
        let text = " map.get(k) {";
        assert_eq!(&text[..scrutinee_end(text)], " map.get(k) ");
    }

    #[test]
    fn finds_the_statement_end() {
        assert_eq!(statement_end(" Foo { a: [1; 2] };"), Some(18));
        assert_eq!(statement_end(" Foo {"), None);
        assert_eq!(statement_end(" \"a;b\".len();"), Some(12));
    }
}

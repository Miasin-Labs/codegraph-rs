//! Written Rust types, read far enough to name the type methods run on.
//!
//! `&'a mut Foo<Bar>` runs `Foo`'s methods, and so do `Box<Foo>`, `Arc<Foo>`,
//! and the other smart pointers that `Deref` to what they wrap. `dyn Trait`
//! and `impl Trait` run the trait's. Slices, arrays, tuples, and pointers are
//! never project types, so they get placeholder names no project type has.

/// Wrappers whose method calls auto-deref to the first type argument.
const DEREF_WRAPPERS: &[&str] = &[
    "Arc",
    "Box",
    "Cow",
    "Lazy",
    "LazyCell",
    "LazyLock",
    "ManuallyDrop",
    "MutexGuard",
    "Pin",
    "Rc",
    "Ref",
    "RefMut",
    "RwLockReadGuard",
    "RwLockWriteGuard",
];

/// Placeholder names for the structural types (`[T]`, `(A, B)`, `*const T`).
pub(super) const SLICE: &str = "[slice]";
const TUPLE: &str = "(tuple)";
const POINTER: &str = "*pointer";

pub(super) fn is_deref_wrapper(name: &str) -> bool {
    DEREF_WRAPPERS.contains(&name)
}

pub(super) fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

pub(super) fn is_ident(text: &str) -> bool {
    !text.is_empty() && !text.as_bytes()[0].is_ascii_digit() && text.bytes().all(is_ident_byte)
}

pub(super) fn starts_uppercase(text: &str) -> bool {
    text.chars().next().is_some_and(char::is_uppercase)
}

/// Strip one leading keyword (`mut`, `dyn`, `impl`) followed by whitespace.
fn strip_keyword<'a>(text: &'a str, keyword: &str) -> Option<&'a str> {
    let rest = text.strip_prefix(keyword)?;
    rest.starts_with(|c: char| c.is_whitespace())
        .then(|| rest.trim_start())
}

/// Drop leading references, lifetimes, and `mut`/`dyn`/`impl`.
pub(super) fn strip_type_prefixes(mut text: &str) -> &str {
    loop {
        text = text.trim_start();
        if let Some(rest) = text.strip_prefix('&') {
            text = rest;
        } else if let Some(rest) = text.strip_prefix('\'') {
            text = rest.trim_start_matches(|c: char| c.is_ascii_alphanumeric() || c == '_');
        } else if let Some(rest) = ["mut", "dyn", "impl"]
            .iter()
            .find_map(|keyword| strip_keyword(text, keyword))
        {
            text = rest;
        } else {
            return text;
        }
    }
}

/// The byte index of the bracket closing the one that opens `text`, counting
/// `()`, `[]`, `{}`, and `<>` (the `>` of `->` is not a bracket).
pub(super) fn matching_close(text: &str) -> Option<usize> {
    closing(text, true)
}

/// [`matching_close`] for expressions, where `<` and `>` compare.
pub(super) fn matching_paren(text: &str) -> Option<usize> {
    closing(text, false)
}

fn closing(text: &str, angles: bool) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => index = skip_string(bytes, index),
            b'<' if !angles => {}
            b'>' if !angles => {}
            b'(' | b'[' | b'{' | b'<' => depth += 1,
            b'-' if bytes.get(index + 1) == Some(&b'>') => index += 1,
            b')' | b']' | b'}' | b'>' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

/// The index of the closing quote of the string literal opening at `start`.
fn skip_string(bytes: &[u8], start: usize) -> usize {
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 1,
            b'"' => return index,
            _ => {}
        }
        index += 1;
    }
    index
}

/// Split `text` on `separator` where no bracket or string is open.
pub(super) fn split_top_level(text: &str, separator: u8) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => index = skip_string(bytes, index),
            b'(' | b'[' | b'{' | b'<' => depth += 1,
            b'-' if bytes.get(index + 1) == Some(&b'>') => index += 1,
            b')' | b']' | b'}' | b'>' => depth = depth.saturating_sub(1),
            byte if byte == separator && depth == 0 => {
                parts.push(text[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
        index += 1;
    }
    parts.push(text[start..].trim());
    parts
}

/// `a::b::Foo<X, Y>` -> (`a::b::Foo`, [`X`, `Y`]). The path ends at the first
/// byte that cannot continue it (`<`, whitespace, `+`, `(`, ...).
pub(super) fn split_path(text: &str) -> (&str, Vec<&str>) {
    let bytes = text.as_bytes();
    let mut end = 0;
    while end < bytes.len() {
        if is_ident_byte(bytes[end]) {
            end += 1;
        } else if text[end..].starts_with("::") && !text[end + 2..].starts_with('<') {
            end += 2;
        } else {
            break;
        }
    }
    let path = &text[..end];
    let rest = text[end..].trim_start_matches("::");
    let args = match rest.strip_prefix('<') {
        Some(_) => matching_close(rest)
            .map(|close| split_top_level(&rest[1..close], b','))
            .unwrap_or_default(),
        None => Vec::new(),
    };
    (path, args)
}

/// The type whose methods a value of written type `written` runs.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Named<'a> {
    /// `Self`.
    SelfType,
    /// A path as written, generic arguments dropped (`tree_sitter::Node`).
    Path(&'a str),
    /// A slice, array, tuple, or raw pointer: never a project type.
    Structural(&'static str),
}

pub(super) fn named_type(written: &str) -> Option<Named<'_>> {
    let text = strip_type_prefixes(written);
    match text.as_bytes().first()? {
        b'[' => return Some(Named::Structural(SLICE)),
        b'(' => return Some(Named::Structural(TUPLE)),
        b'*' => return Some(Named::Structural(POINTER)),
        _ => {}
    }
    let (path, args) = split_path(text);
    let last = path.rsplit("::").next()?;
    if !is_ident(last) || last == "_" {
        return None;
    }
    if last == "Self" {
        return Some(Named::SelfType);
    }
    if is_deref_wrapper(last) {
        let inner = args.into_iter().find(|arg| !arg.starts_with('\''))?;
        return named_type(inner);
    }
    Some(Named::Path(path))
}

/// The outer type of `written` when it is `Result<T, ..>` or `Option<T>`
/// (under any path, `io::Result<T>` included), or the `LockResult<T>` a
/// `lock()` returns: `T` as written.
pub(super) fn unwrapped(written: &str) -> Option<&str> {
    let text = peeled(written);
    let (path, args) = split_path(text);
    let last = path.rsplit("::").next()?;
    matches!(last, "Result" | "Option" | "LockResult" | "TryLockResult")
        .then(|| type_args(&args).first().copied())
        .flatten()
}

/// `written` without references and the smart pointers that deref to what
/// they wrap, generic arguments kept: `&Arc<RefCell<Graph>>` is
/// `RefCell<Graph>`.
pub(super) fn peeled(written: &str) -> &str {
    let mut text = strip_type_prefixes(written);
    loop {
        let (path, args) = split_path(text);
        let last = path.rsplit("::").next().unwrap_or_default();
        let inner = is_deref_wrapper(last)
            .then(|| type_args(&args).first().copied())
            .flatten();
        match inner {
            Some(inner) => text = strip_type_prefixes(inner),
            None => return text,
        }
    }
}

/// The type arguments among `args`, lifetimes dropped.
pub(super) fn type_args<'a>(args: &[&'a str]) -> Vec<&'a str> {
    args.iter()
        .copied()
        .filter(|arg| !arg.starts_with('\''))
        .collect()
}

/// The parameters `(a: A, mut b: &B)` of a signature as (pattern, type).
pub(super) fn signature_params(signature: &str) -> Vec<(&str, &str)> {
    let Some(open) = signature.find('(') else {
        return Vec::new();
    };
    let Some(close) = matching_close(&signature[open..]) else {
        return Vec::new();
    };
    split_top_level(&signature[open + 1..open + close], b',')
        .into_iter()
        .filter_map(|param| {
            let colon = type_colon(param)?;
            let pattern = param[..colon].trim();
            let pattern = strip_keyword(pattern, "mut").unwrap_or(pattern);
            Some((pattern, param[colon + 1..].trim()))
        })
        .collect()
}

/// The return type of a signature, `None` for `()`.
pub(super) fn signature_return(signature: &str) -> Option<&str> {
    let open = signature.find('(')?;
    let close = open + matching_close(&signature[open..])?;
    let rest = signature[close + 1..].trim_start().strip_prefix("->")?;
    let rest = rest.split(" where ").next().unwrap_or(rest);
    let rest = rest.trim().trim_end_matches('{').trim();
    (!rest.is_empty() && rest != "()").then_some(rest)
}

/// The `:` that separates a binding from its type (not a `::` path).
pub(super) fn type_colon(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    (0..bytes.len()).find(|&index| {
        bytes[index] == b':'
            && bytes.get(index + 1) != Some(&b':')
            && (index == 0 || bytes[index - 1] != b':')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_type_methods_run_on() {
        let cases = [
            ("&'a mut Foo<Bar>", Some(Named::Path("Foo"))),
            (
                "crate::graph::Graph",
                Some(Named::Path("crate::graph::Graph")),
            ),
            ("Box<dyn Visitor + Send>", Some(Named::Path("Visitor"))),
            ("Arc<Mutex<State>>", Some(Named::Path("Mutex"))),
            ("Cow<'a, str>", Some(Named::Path("str"))),
            ("impl Iterator<Item = &Node>", Some(Named::Path("Iterator"))),
            ("&[Node]", Some(Named::Structural(SLICE))),
            ("(u32, Foo)", Some(Named::Structural(TUPLE))),
            ("Vec<_>", Some(Named::Path("Vec"))),
            ("_", None),
            ("&mut Self", Some(Named::SelfType)),
        ];
        for (written, expected) in cases {
            assert_eq!(named_type(written), expected, "{written}");
        }
    }

    #[test]
    fn reads_signature_params_and_return_type() {
        let signature = "(&self, mut nodes: Vec<Node>, f: impl Fn(u32) -> u32, (a, b): (A, B)) -> io::Result<Self>";
        assert_eq!(
            signature_params(signature),
            [
                ("nodes", "Vec<Node>"),
                ("f", "impl Fn(u32) -> u32"),
                ("(a, b)", "(A, B)")
            ]
        );
        let ret = signature_return(signature).unwrap();
        assert_eq!(ret, "io::Result<Self>");
        assert_eq!(unwrapped(ret), Some("Self"));
        assert_eq!(signature_return("(x: u32)"), None);
    }
}

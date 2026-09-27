//! Rust signatures as the harness sees them: the parameter list split from
//! the index's `signature`, each type classified into what a fuzzer can
//! produce and how to pass it.

use std::collections::HashMap;

use crate::analyze::fuzz::model::InputShape;

/// How a method takes its receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelfKind {
    Ref,
    Mut,
    Value,
}

/// One parameter of the index's signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawParam {
    SelfParam(SelfKind),
    Named { name: String, ty: String },
}

/// How the harness passes a value it built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgForm {
    /// `&[u8]`, `impl AsRef<[u8]>`, `B: AsRef<[u8]>`.
    BytesSlice,
    /// `&mut [u8]`.
    BytesSliceMut,
    /// `Vec<u8>` by value.
    BytesVec,
    /// `&Vec<u8>`.
    BytesVecRef,
    /// `Box<[u8]>`, `Cow<[u8]>`, `impl Into<Vec<u8>>`: from a `Vec<u8>`.
    BytesInto,
    /// `&str`, `impl AsRef<str>`.
    Str,
    /// `String` by value, `impl ToString`.
    StringOwned,
    /// `&String`.
    StringRef,
    /// `Cow<str>`, `impl Into<String>`: from a `String`.
    StringInto,
    /// `impl Read`, `R: BufRead` (a cursor by value) or `&mut R`,
    /// `&mut dyn Read` (by reference).
    Reader { by_ref: bool },
    /// An `Arbitrary` value, held in the input struct as `field`.
    Value { field: String, pass: Pass },
    /// A project type with a `Default`: `Default::default()`, by value or
    /// behind a (mutable) reference.
    Default { reference: &'static str },
    /// Nothing the harness can build.
    Opaque,
}

/// How an [`ArgForm::Value`] reaches the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    Owned,
    Ref,
    MutRef,
    /// `&[T]` from a `Vec<T>` field.
    Slice,
    MutSlice,
}

impl ArgForm {
    pub fn shape(&self) -> InputShape {
        match self {
            Self::BytesSlice
            | Self::BytesSliceMut
            | Self::BytesVec
            | Self::BytesVecRef
            | Self::BytesInto => InputShape::Bytes,
            Self::Str | Self::StringOwned | Self::StringRef | Self::StringInto => InputShape::Text,
            Self::Reader { .. } => InputShape::Reader,
            Self::Value { .. } => InputShape::Structured,
            Self::Default { .. } => InputShape::Fixed,
            Self::Opaque => InputShape::Opaque,
        }
    }

    /// The type of the input-struct field that holds it.
    pub fn field_type(&self) -> Option<String> {
        Some(match self {
            Self::BytesSlice
            | Self::BytesSliceMut
            | Self::BytesVec
            | Self::BytesVecRef
            | Self::BytesInto
            | Self::Reader { .. } => "Vec<u8>".to_string(),
            Self::Str | Self::StringOwned | Self::StringRef | Self::StringInto => {
                "String".to_string()
            }
            Self::Value { field, .. } => field.clone(),
            Self::Default { .. } | Self::Opaque => return None,
        })
    }

    /// Whether passing it borrows the field mutably.
    pub fn needs_mut(&self) -> bool {
        matches!(
            self,
            Self::BytesSliceMut
                | Self::Value {
                    pass: Pass::MutRef | Pass::MutSlice,
                    ..
                }
        )
    }

    /// The argument, from a field expression (`input.x`).
    pub fn from_field(&self, field: &str) -> String {
        match self {
            Self::BytesSlice => format!("{field}.as_slice()"),
            Self::BytesSliceMut => format!("{field}.as_mut_slice()"),
            Self::BytesVec | Self::StringOwned => field.to_string(),
            Self::BytesVecRef | Self::StringRef => format!("&{field}"),
            Self::BytesInto | Self::StringInto => format!("{field}.into()"),
            Self::Str => format!("{field}.as_str()"),
            Self::Reader { by_ref: false } => format!("std::io::Cursor::new({field}.as_slice())"),
            Self::Reader { by_ref: true } => {
                format!("&mut std::io::Cursor::new({field}.as_slice())")
            }
            Self::Value { pass, .. } => match pass {
                Pass::Owned => field.to_string(),
                Pass::Ref => format!("&{field}"),
                Pass::MutRef => format!("&mut {field}"),
                Pass::Slice => format!("{field}.as_slice()"),
                Pass::MutSlice => format!("{field}.as_mut_slice()"),
            },
            Self::Default { reference } => format!("{reference}Default::default()"),
            Self::Opaque => "todo!()".to_string(),
        }
    }

    /// The argument straight from the fuzzer's buffer (`data: &[u8]`), or
    /// from `text: &str` when [`ArgForm::shape`] is text.
    pub fn from_buffer(&self) -> Option<String> {
        Some(match self {
            Self::BytesSlice => "data".to_string(),
            Self::BytesSliceMut => "&mut data.to_vec()".to_string(),
            Self::BytesVec => "data.to_vec()".to_string(),
            Self::BytesVecRef => "&data.to_vec()".to_string(),
            Self::BytesInto => "data.to_vec().into()".to_string(),
            Self::Str => "text".to_string(),
            Self::StringOwned => "text.to_string()".to_string(),
            Self::StringRef => "&text.to_string()".to_string(),
            Self::StringInto => "text.to_string().into()".to_string(),
            Self::Reader { by_ref: false } => "std::io::Cursor::new(data)".to_string(),
            Self::Reader { by_ref: true } => "&mut std::io::Cursor::new(data)".to_string(),
            Self::Default { reference } => format!("{reference}Default::default()"),
            Self::Value { .. } | Self::Opaque => return None,
        })
    }
}

/// The text between the signature's outer parentheses, split at top-level
/// commas.
pub fn split_params(signature: &str) -> Vec<RawParam> {
    let Some(inner) = paren_contents(signature) else {
        return Vec::new();
    };
    split_top_level(inner, ',')
        .into_iter()
        .map(|param| strip_attributes(param.trim()))
        .filter(|param| !param.is_empty())
        .map(|param| parse_param(&param))
        .collect()
}

/// The return type after the parameter list, if any (`-> T`).
pub fn return_type(signature: &str) -> Option<String> {
    let start = signature.find('(')?;
    let close = matching_close(signature, start)?;
    let rest = signature[close + 1..].trim();
    let rest = rest.strip_prefix("->")?.trim();
    let rest = match rest.find(" where ") {
        Some(at) => &rest[..at],
        None => rest,
    };
    Some(normalize(rest))
}

fn paren_contents(signature: &str) -> Option<&str> {
    let start = signature.find('(')?;
    let close = matching_close(signature, start)?;
    Some(&signature[start + 1..close])
}

fn matching_close(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let bytes = text.as_bytes();
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' | b'<' => depth += 1,
            b'-' if bytes.get(i + 1) == Some(&b'>') => i += 1,
            b')' | b']' | b'}' | b'>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Split at `sep` outside brackets (`->` is not a closing angle).
pub fn split_top_level(text: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        match byte {
            b'(' | b'[' | b'{' | b'<' => depth += 1,
            b'-' if bytes.get(i + 1) == Some(&b'>') => i += 1,
            b')' | b']' | b'}' | b'>' => depth -= 1,
            _ if depth == 0 && byte == sep as u8 => {
                parts.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push(&text[start..]);
    parts
}

fn strip_attributes(param: &str) -> String {
    let mut rest = param.trim();
    while rest.starts_with("#[") {
        let Some(close) = matching_close(rest, 1) else {
            break;
        };
        rest = rest[close + 1..].trim_start();
    }
    rest.to_string()
}

fn parse_param(param: &str) -> RawParam {
    let compact = normalize(param);
    let receiver = compact
        .split_once(':')
        .map_or(compact.as_str(), |(pattern, _)| pattern)
        .trim();
    let receiver_type = compact.split_once(':').map(|(_, ty)| ty.trim());
    let self_kind = match receiver {
        "self" | "mut self" => Some(match receiver_type {
            Some(ty) if ty.starts_with("&mut") => SelfKind::Mut,
            Some(ty) if ty.starts_with('&') => SelfKind::Ref,
            _ => SelfKind::Value,
        }),
        "&self" => Some(SelfKind::Ref),
        "&mut self" => Some(SelfKind::Mut),
        other if other.starts_with("&'") && other.ends_with(" self") => {
            Some(if other.contains(" mut ") {
                SelfKind::Mut
            } else {
                SelfKind::Ref
            })
        }
        _ => None,
    };
    if let Some(kind) = self_kind {
        return RawParam::SelfParam(kind);
    }
    // `pattern: Type` — the first `:` that is not part of `::`.
    let bytes = compact.as_bytes();
    let mut split = None;
    for (i, &byte) in bytes.iter().enumerate() {
        if byte == b':' && bytes.get(i + 1) != Some(&b':') && (i == 0 || bytes[i - 1] != b':') {
            split = Some(i);
            break;
        }
    }
    match split {
        Some(at) => {
            let pattern = compact[..at].trim();
            let name = pattern
                .trim_start_matches("mut ")
                .trim_start_matches("ref ")
                .to_string();
            RawParam::Named {
                name,
                ty: compact[at + 1..].trim().to_string(),
            }
        }
        None => RawParam::Named {
            name: "_".into(),
            ty: compact,
        },
    }
}

/// Collapse whitespace; drop spaces just inside brackets and before commas.
pub fn normalize(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let joined = words.join(" ");
    joined
        .replace("< ", "<")
        .replace(" >", ">")
        .replace("( ", "(")
        .replace(" )", ")")
        .replace("[ ", "[")
        .replace(" ]", "]")
        .replace(" ,", ",")
        .replace(",)", ")")
        .replace(",>", ">")
}

/// Generic parameter bounds from `<R: Read, T>` and `where R: BufRead`:
/// name → bound text (`Read + Send`).
pub fn generic_bounds(generics: &str) -> HashMap<String, String> {
    let mut bounds: HashMap<String, String> = HashMap::new();
    let text = normalize(generics);
    let where_at = if text.starts_with("where ") {
        Some(0)
    } else {
        text.find(" where ").map(|at| at + 1)
    };
    let (params, clause) = match where_at {
        Some(at) => (&text[..at], &text[at + "where".len()..]),
        None => (text.as_str(), ""),
    };
    let params = params.trim();
    let params = params
        .strip_prefix('<')
        .and_then(|p| p.strip_suffix('>'))
        .unwrap_or(params);
    for part in split_top_level(params, ',')
        .into_iter()
        .chain(split_top_level(clause, ','))
    {
        let part = part.trim();
        if part.is_empty() || part.starts_with('\'') || part.starts_with("const ") {
            continue;
        }
        let (name, bound) = match split_bound(part) {
            Some((name, bound)) => (name, bound),
            None => (part, ""),
        };
        let entry = bounds.entry(name.trim().to_string()).or_default();
        if !bound.trim().is_empty() {
            if !entry.is_empty() {
                entry.push_str(" + ");
            }
            entry.push_str(bound.trim());
        }
    }
    bounds
}

fn split_bound(part: &str) -> Option<(&str, &str)> {
    let bytes = part.as_bytes();
    let mut depth = 0i32;
    for (i, &byte) in bytes.iter().enumerate() {
        match byte {
            b'<' | b'(' | b'[' => depth += 1,
            b'>' | b')' | b']' => depth -= 1,
            b':' if depth == 0
                && bytes.get(i + 1) != Some(&b':')
                && (i == 0 || bytes[i - 1] != b':') =>
            {
                return Some((&part[..i], &part[i + 1..]));
            }
            _ => {}
        }
    }
    None
}

/// Bounds `u8` meets: a type parameter bounded only by these can be `u8`.
const U8_BOUNDS: &[&str] = &[
    "Copy",
    "Clone",
    "Default",
    "Debug",
    "Display",
    "PartialEq",
    "Eq",
    "PartialOrd",
    "Ord",
    "Hash",
    "Send",
    "Sync",
    "Sized",
    "Unpin",
    "'static",
];

/// Type parameters (of `bounds`) a harness can instantiate with `u8`:
/// unbounded, or bounded only by traits `u8` implements.
pub fn u8_params(bounds: &HashMap<String, String>) -> Vec<String> {
    let mut names: Vec<String> = bounds
        .iter()
        .filter(|(_, bound)| {
            bound_words(bound)
                .filter(|word| !word.is_empty())
                .all(|word| U8_BOUNDS.contains(&word))
        })
        .map(|(name, _)| name.clone())
        .collect();
    names.sort();
    names
}

/// `ty` with the type parameter `name` (a whole word) replaced by `with`.
pub fn substitute(ty: &str, name: &str, with: &str) -> String {
    let mut out = String::new();
    let mut word = String::new();
    for ch in ty.chars().chain(std::iter::once('\0')) {
        if ch.is_alphanumeric() || ch == '_' {
            word.push(ch);
            continue;
        }
        out.push_str(if word == name { with } else { &word });
        word.clear();
        if ch != '\0' {
            out.push(ch);
        }
    }
    out
}

const SCALARS: &[&str] = &[
    "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize", "f32",
    "f64", "bool", "char",
];

/// The project type a parameter names, and how it is referenced:
/// `&mut opts::Config<'a>` → (`"&mut "`, `Config`).
pub fn named_type(ty: &str) -> Option<(&'static str, String)> {
    let ty = canonical_paths(&normalize(ty));
    let (reference, inner) = strip_reference(&ty);
    let head = inner.split('<').next().unwrap_or(inner).trim();
    let name = head.rsplit("::").next().unwrap_or(head);
    let is_path = !name.is_empty()
        && name.starts_with(|c: char| c.is_ascii_uppercase())
        && name.chars().all(|c| c.is_alphanumeric() || c == '_');
    let reference = match reference {
        Reference::None => "",
        Reference::Shared => "&",
        Reference::Mut => "&mut ",
    };
    is_path.then(|| (reference, name.to_string()))
}

/// Classify one parameter type. `bounds` holds the function's generic
/// parameters (name → bounds).
pub fn classify(ty: &str, bounds: &HashMap<String, String>) -> ArgForm {
    let ty = canonical_paths(&normalize(ty));
    let (reference, inner) = strip_reference(&ty);
    match reference {
        Reference::Mut => classify_mut_ref(inner, bounds),
        Reference::Shared => classify_ref(inner, bounds),
        Reference::None => classify_owned(inner, bounds),
    }
}

/// Spell std's paths the way the prelude does (`std::vec::Vec` → `Vec`).
fn canonical_paths(ty: &str) -> String {
    let mut out = ty.to_string();
    for (long, short) in [
        ("std::vec::Vec", "Vec"),
        ("alloc::vec::Vec", "Vec"),
        ("std::string::String", "String"),
        ("alloc::string::String", "String"),
        ("std::borrow::Cow", "Cow"),
        ("alloc::borrow::Cow", "Cow"),
        ("std::boxed::Box", "Box"),
        ("alloc::boxed::Box", "Box"),
        ("std::option::Option", "Option"),
        ("core::option::Option", "Option"),
    ] {
        out = out.replace(long, short);
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reference {
    None,
    Shared,
    Mut,
}

fn strip_reference(ty: &str) -> (Reference, &str) {
    let Some(rest) = ty.strip_prefix('&') else {
        return (Reference::None, ty);
    };
    let mut rest = rest.trim_start();
    if rest.starts_with('\'') {
        // A lifetime: skip to the end of its name.
        let end = rest.find(' ').unwrap_or(rest.len());
        rest = rest[end..].trim_start();
    }
    match rest.strip_prefix("mut ") {
        Some(inner) => (Reference::Mut, inner.trim()),
        None => (Reference::Shared, rest),
    }
}

fn bytes_slice(ty: &str) -> bool {
    ty == "[u8]"
}

/// `[T]` of an `Arbitrary` element (not `[u8]`, which is bytes).
fn slice_element(ty: &str) -> Option<&str> {
    ty.strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .filter(|element| !element.contains(';') && is_arbitrary(element))
}

fn classify_ref(inner: &str, bounds: &HashMap<String, String>) -> ArgForm {
    if bytes_slice(inner) {
        return ArgForm::BytesSlice;
    }
    if inner == "str" {
        return ArgForm::Str;
    }
    if inner == "Vec<u8>" {
        return ArgForm::BytesVecRef;
    }
    if inner == "String" {
        return ArgForm::StringRef;
    }
    // `&B` with `B: AsRef<[u8]>`: `B` must be sized, so a `Vec<u8>`.
    if let Some(bound) = bounds.get(inner) {
        if bound_is_bytes(bound) {
            return ArgForm::BytesVecRef;
        }
        if bound_is_text(bound) {
            return ArgForm::StringRef;
        }
    }
    if let Some(element) = slice_element(inner) {
        return ArgForm::Value {
            field: format!("Vec<{element}>"),
            pass: Pass::Slice,
        };
    }
    if is_arbitrary(inner) {
        return ArgForm::Value {
            field: inner.to_string(),
            pass: Pass::Ref,
        };
    }
    ArgForm::Opaque
}

fn classify_mut_ref(inner: &str, bounds: &HashMap<String, String>) -> ArgForm {
    if bytes_slice(inner) {
        return ArgForm::BytesSliceMut;
    }
    if is_reader_type(inner) || bounds.get(inner).is_some_and(|b| bound_is_reader(b)) {
        return ArgForm::Reader { by_ref: true };
    }
    if let Some(element) = slice_element(inner) {
        return ArgForm::Value {
            field: format!("Vec<{element}>"),
            pass: Pass::MutSlice,
        };
    }
    if is_arbitrary(inner) {
        return ArgForm::Value {
            field: inner.to_string(),
            pass: Pass::MutRef,
        };
    }
    ArgForm::Opaque
}

fn classify_owned(ty: &str, bounds: &HashMap<String, String>) -> ArgForm {
    if ty == "Vec<u8>" {
        return ArgForm::BytesVec;
    }
    if ty == "String" {
        return ArgForm::StringOwned;
    }
    if ty == "Box<[u8]>" || is_cow_of(ty, "[u8]") || ty == "bytes::Bytes" || ty == "Bytes" {
        // `Bytes: From<Vec<u8>>`.
        return ArgForm::BytesInto;
    }
    if is_cow_of(ty, "str") || ty == "Box<str>" {
        return ArgForm::StringInto;
    }
    if let Some(bound) = ty.strip_prefix("impl ") {
        return classify_bound(bound).unwrap_or(ArgForm::Opaque);
    }
    if let Some(bound) = bounds.get(ty) {
        return classify_bound(bound).unwrap_or(ArgForm::Opaque);
    }
    if is_arbitrary(ty) {
        return ArgForm::Value {
            field: ty.to_string(),
            pass: Pass::Owned,
        };
    }
    ArgForm::Opaque
}

/// A value of a type known only by its bounds (`impl Read`, `R: AsRef<[u8]>`).
fn classify_bound(bound: &str) -> Option<ArgForm> {
    if bound_is_reader(bound) {
        return Some(ArgForm::Reader { by_ref: false });
    }
    if bound_is_bytes(bound) {
        return Some(ArgForm::BytesSlice);
    }
    if bound.contains("Into<Vec<u8>>") {
        return Some(ArgForm::BytesInto);
    }
    if bound_is_text(bound) {
        return Some(ArgForm::Str);
    }
    if bound.contains("Into<String>") || bound.contains("ToString") {
        return Some(ArgForm::StringOwned);
    }
    None
}

fn is_cow_of(ty: &str, inner: &str) -> bool {
    let Some(args) = ty
        .strip_prefix("Cow<")
        .or_else(|| ty.strip_prefix("std::borrow::Cow<"))
        .and_then(|s| s.strip_suffix('>'))
    else {
        return false;
    };
    args.rsplit(',').next().map(str::trim) == Some(inner)
}

fn bound_words(bound: &str) -> impl Iterator<Item = &str> {
    bound
        .split('+')
        .map(|word| word.trim().trim_start_matches('?'))
        .map(|word| word.rsplit("::").next().unwrap_or(word))
}

fn bound_is_reader(bound: &str) -> bool {
    bound_words(bound).any(|word| matches!(word, "Read" | "BufRead"))
}

fn bound_is_bytes(bound: &str) -> bool {
    bound_words(bound).any(|word| word == "AsRef<[u8]>")
}

fn bound_is_text(bound: &str) -> bool {
    bound_words(bound).any(|word| word == "AsRef<str>")
}

fn is_reader_type(ty: &str) -> bool {
    let ty = ty.trim();
    let bound = ty
        .strip_prefix("dyn ")
        .or_else(|| ty.strip_prefix("impl "))
        .unwrap_or("");
    !bound.is_empty() && bound_is_reader(bound)
}

/// Types `arbitrary` builds: scalars, `String`, and `Vec`/`Option`/`Box`,
/// arrays and tuples of them.
pub fn is_arbitrary(ty: &str) -> bool {
    let ty = ty.trim();
    if SCALARS.contains(&ty) || ty == "String" || ty == "()" {
        return true;
    }
    for wrapper in ["Vec<", "Option<", "Box<", "std::vec::Vec<"] {
        if let Some(inner) = ty.strip_prefix(wrapper).and_then(|s| s.strip_suffix('>')) {
            return is_arbitrary(inner);
        }
    }
    if let Some((element, len)) = ty
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .and_then(|inner| inner.rsplit_once(';'))
    {
        return is_arbitrary(element) && len.trim().parse::<usize>().is_ok();
    }
    if let Some(inner) = ty.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
        return split_top_level(inner, ',')
            .into_iter()
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .all(is_arbitrary);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(ty: &str) -> ArgForm {
        classify(ty, &HashMap::new())
    }

    #[test]
    fn splits_parameters_and_receivers() {
        let params = split_params(
            "(&mut self, data: &[u8], f: impl Fn(u8) -> u8, (a, b): (u8, u8)) -> Result<(), E>",
        );
        assert_eq!(params[0], RawParam::SelfParam(SelfKind::Mut));
        assert_eq!(
            params[1],
            RawParam::Named {
                name: "data".into(),
                ty: "&[u8]".into()
            }
        );
        assert_eq!(
            params[2],
            RawParam::Named {
                name: "f".into(),
                ty: "impl Fn(u8) -> u8".into()
            }
        );
        assert_eq!(params.len(), 4);
        assert_eq!(
            split_params("(self)"),
            vec![RawParam::SelfParam(SelfKind::Value)]
        );
        assert_eq!(
            split_params("(&'a self, x: u8)")[0],
            RawParam::SelfParam(SelfKind::Ref)
        );
        assert_eq!(
            split_params("(\n    mut buf: std::vec::Vec<u8>,\n)"),
            vec![RawParam::Named {
                name: "buf".into(),
                ty: "std::vec::Vec<u8>".into()
            }]
        );
        assert!(split_params("()").is_empty());
    }

    #[test]
    fn return_types_drop_where_clauses() {
        assert_eq!(
            return_type("(s: &str) -> Result<Self, Error>").as_deref(),
            Some("Result<Self, Error>")
        );
        assert_eq!(return_type("(f: impl Fn() -> u8)"), None);
        assert_eq!(
            return_type("(r: R) -> io::Result<u8> where R: Read").as_deref(),
            Some("io::Result<u8>")
        );
    }

    #[test]
    fn classifies_bytes_text_and_readers() {
        assert_eq!(class("&[u8]"), ArgForm::BytesSlice);
        assert_eq!(class("&'a [u8]"), ArgForm::BytesSlice);
        assert_eq!(class("&mut [u8]"), ArgForm::BytesSliceMut);
        assert_eq!(class("Vec<u8>"), ArgForm::BytesVec);
        assert_eq!(class("&str"), ArgForm::Str);
        assert_eq!(class("String"), ArgForm::StringOwned);
        assert_eq!(class("Cow<'a, str>"), ArgForm::StringInto);
        assert_eq!(class("impl Read"), ArgForm::Reader { by_ref: false });
        assert_eq!(
            class("&mut dyn std::io::Read"),
            ArgForm::Reader { by_ref: true }
        );
        assert_eq!(class("impl AsRef<[u8]>"), ArgForm::BytesSlice);
        let bounds = generic_bounds("<R: io::BufRead, B> where B: AsRef<[u8]>");
        assert_eq!(classify("R", &bounds), ArgForm::Reader { by_ref: false });
        assert_eq!(
            classify("&mut R", &bounds),
            ArgForm::Reader { by_ref: true }
        );
        assert_eq!(classify("B", &bounds), ArgForm::BytesSlice);
        assert_eq!(classify("&B", &bounds), ArgForm::BytesVecRef);
        assert_eq!(class("std::vec::Vec<u8>"), ArgForm::BytesVec);
        let only_where = generic_bounds("where R: Read");
        assert_eq!(
            classify("R", &only_where),
            ArgForm::Reader { by_ref: false }
        );
    }

    #[test]
    fn classifies_arbitrary_values_and_opaque_types() {
        assert_eq!(
            class("u32"),
            ArgForm::Value {
                field: "u32".into(),
                pass: Pass::Owned
            }
        );
        assert_eq!(
            class("&[u16]"),
            ArgForm::Value {
                field: "Vec<u16>".into(),
                pass: Pass::Slice
            }
        );
        assert_eq!(
            class("Option<(u8, bool)>"),
            ArgForm::Value {
                field: "Option<(u8, bool)>".into(),
                pass: Pass::Owned
            }
        );
        assert_eq!(
            class("[u8; 4]"),
            ArgForm::Value {
                field: "[u8; 4]".into(),
                pass: Pass::Owned
            }
        );
        assert_eq!(class("&mut decode::Content<S>"), ArgForm::Opaque);
        assert_eq!(class("Config"), ArgForm::Opaque);
        assert_eq!(class("T"), ArgForm::Opaque);
    }

    #[test]
    fn plain_type_parameters_become_u8() {
        let bounds = generic_bounds("<T: Copy + core::fmt::Debug, R: Read, U>");
        assert_eq!(u8_params(&bounds), vec!["T".to_string(), "U".to_string()]);
        assert_eq!(substitute("&mut [T]", "T", "u8"), "&mut [u8]");
        assert_eq!(substitute("Vec<Tree<T>>", "T", "u8"), "Vec<Tree<u8>>");
        assert_eq!(
            classify(&substitute("&[T]", "T", "u8"), &bounds),
            ArgForm::BytesSlice
        );
    }

    #[test]
    fn generic_bounds_merge_params_and_where() {
        let bounds = generic_bounds("<'a, R: Read + Send, T, const N: usize> where T: Default");
        assert_eq!(bounds["R"], "Read + Send");
        assert_eq!(bounds["T"], "Default");
        assert!(!bounds.contains_key("'a"));
    }
}

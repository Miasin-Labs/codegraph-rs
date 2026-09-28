//! The generic parameters in scope at a line of a Rust file.
//!
//! A bare `T` in a type names the enclosing item's parameter, never a
//! project type that happens to be called `T` (a test's `struct R;`, an
//! `enum E`): rustc resolves a generic parameter before any module item.
//! The index keeps no generics, so each file's items that declare them
//! (`fn f<T>`, `impl<R: Read>`, `struct S<W>`, `trait Tr<A>`, `enum`,
//! `union`, `type`) are read from its text once, with the lines each item
//! spans, and kept per file by the resolution context
//! ([`ResolutionContext::get_rust_file_derived`]).

use std::collections::HashMap;
use std::sync::Arc;

use crate::resolution::types::{ResolutionContext, UnresolvedRef};

/// What the context keeps per file.
type FileDerived = Arc<dyn std::any::Any + Send + Sync>;

/// Each generic parameter name of a file, with the disjoint line ranges
/// (1-based, inclusive) of the items declaring it.
#[derive(Debug, Default)]
struct GenericScopes {
    by_name: HashMap<String, Vec<(u32, u32)>>,
    /// Each declaration of a parameter: the declaring item's lines and the
    /// trait bounds it writes (`T: A + B`, `where T: C`), paths as written
    /// without generic arguments. Sorted by first line.
    bounds: HashMap<String, Vec<Declared>>,
}

#[derive(Debug, Clone)]
struct Declared {
    start: u32,
    end: u32,
    bounds: Vec<String>,
}

impl GenericScopes {
    fn contains(&self, name: &str, line: u32) -> bool {
        let Some(ranges) = self.by_name.get(name) else {
            return false;
        };
        let at = ranges.partition_point(|&(start, _)| start <= line);
        at > 0 && ranges[at - 1].1 >= line
    }

    /// The bounds of the innermost declaration of `name` around `line`.
    fn bounds(&self, name: &str, line: u32) -> Option<&[String]> {
        self.bounds
            .get(name)?
            .iter()
            .filter(|declared| declared.start <= line && line <= declared.end)
            .max_by_key(|declared| declared.start)
            .map(|declared| declared.bounds.as_slice())
    }
}

/// Whether `name` is a generic parameter of an item enclosing the
/// reference's line.
pub(super) fn is_generic_param(
    name: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> bool {
    let scopes = file_scopes(&reference.file_path, context);
    scopes
        .downcast_ref::<GenericScopes>()
        .is_some_and(|scopes| scopes.contains(name, reference.line))
}

/// The trait bounds of the generic parameter `name` declared by the
/// innermost item enclosing the reference's line (`R` in `impl<R: AsyncRead>
/// … where R: Unpin` -> [`AsyncRead`, `Unpin`]), as written. `None` when
/// `name` is no generic parameter there.
pub(in crate::resolution::name_matcher) fn generic_bounds(
    name: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<Vec<String>> {
    let scopes = file_scopes(&reference.file_path, context);
    scopes
        .downcast_ref::<GenericScopes>()?
        .bounds(name, reference.line)
        .map(<[String]>::to_vec)
}

/// The supertraits `trait Name: A + B<X> { … }` in `file` writes for its
/// trait `name` (paths as written), scanned once per file.
pub(in crate::resolution::name_matcher) fn supertraits(
    name: &str,
    file: &str,
    context: &dyn ResolutionContext,
) -> Vec<String> {
    let derived = context.get_rust_file_derived(file, "rust-supertraits", &mut || {
        let found = context
            .read_file_arc(file)
            .map(|source| scan_supertraits(&source))
            .unwrap_or_default();
        Arc::new(found)
    });
    derived
        .downcast_ref::<HashMap<String, Vec<String>>>()
        .and_then(|found| found.get(name))
        .cloned()
        .unwrap_or_default()
}

/// Each `trait Name<..>: bounds` of a file: name -> bound paths.
fn scan_supertraits(source: &str) -> HashMap<String, Vec<String>> {
    let code = Code::of(source);
    let bytes = code.bytes.as_slice();
    let text = code.text();
    let mut found: HashMap<String, Vec<String>> = HashMap::new();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i..].starts_with(b"trait")
            || (i > 0 && is_ident(bytes[i - 1]))
            || bytes
                .get(i + 5)
                .is_none_or(|&byte| !byte.is_ascii_whitespace())
        {
            i += 1;
            continue;
        }
        let name_start = skip_space(bytes, i + 5);
        let name_end = ident_end(bytes, name_start);
        i = name_end.max(i + 5);
        if name_end == name_start {
            continue;
        }
        let mut at = skip_space(bytes, name_end);
        if bytes.get(at) == Some(&b'<') {
            let Some(close) = closing_angle(bytes, at) else {
                continue;
            };
            at = skip_space(bytes, close + 1);
        }
        if bytes.get(at) != Some(&b':') {
            continue;
        }
        // The bounds run to the body, a `where` clause or a `;`.
        let mut end = at + 1;
        let mut depth = 0i32;
        while end < bytes.len() {
            match bytes[end] {
                b'<' | b'(' => depth += 1,
                b'>' if bytes[end - 1] == b'-' => {}
                b'>' | b')' => depth -= 1,
                b'{' | b';' if depth <= 0 => break,
                b'w' if depth <= 0
                    && bytes[end..].starts_with(b"where")
                    && !is_ident(bytes[end - 1])
                    && !bytes.get(end + 5).is_some_and(|&byte| is_ident(byte)) =>
                {
                    break;
                }
                _ => {}
            }
            end += 1;
        }
        if let (Some(name), Some(bounds)) = (text.get(name_start..name_end), text.get(at + 1..end))
        {
            found
                .entry(name.to_string())
                .or_default()
                .extend(bound_paths(bounds));
        }
        i = end;
    }
    found
}

/// The file's generic scopes, scanned once per file and kept by the
/// resolution context.
fn file_scopes(file: &str, context: &dyn ResolutionContext) -> FileDerived {
    context.get_rust_file_derived(file, "rust-generic-scopes", &mut || {
        let scopes = context
            .read_file_arc(file)
            .map(|source| scan(&source))
            .unwrap_or_default();
        Arc::new(scopes)
    })
}

/// Items that can declare generic parameters.
const GENERIC_ITEMS: &[&str] = &["enum", "fn", "impl", "struct", "trait", "type", "union"];

fn scan(source: &str) -> GenericScopes {
    let code = Code::of(source);
    let mut found: HashMap<String, Vec<(u32, u32)>> = HashMap::new();
    let mut bounds: HashMap<String, Vec<Declared>> = HashMap::new();
    let bytes = code.bytes.as_slice();
    // Validated once: `code.text()` is linear in the file.
    let text = code.text();
    let mut i = 0;
    while i < bytes.len() {
        if !is_ident_start(bytes[i]) || (i > 0 && is_ident(bytes[i - 1])) {
            i += 1;
            continue;
        }
        let word_end = ident_end(bytes, i);
        let word = &source[i..word_end];
        if !GENERIC_ITEMS.contains(&word) {
            i = word_end;
            continue;
        }
        let mut at = skip_space(bytes, word_end);
        if word != "impl" {
            // The item's name, then its parameters.
            if at >= bytes.len() || !is_ident_start(bytes[at]) {
                i = word_end;
                continue;
            }
            at = skip_space(bytes, ident_end(bytes, at));
        }
        if bytes.get(at) != Some(&b'<') {
            i = word_end;
            continue;
        }
        let Some(close) = closing_angle(bytes, at) else {
            i = word_end;
            continue;
        };
        let params = parameter_names(&source[at + 1..close]);
        let end = item_end(bytes, close + 1);
        let (first, last) = (code.line_of(i), code.line_of(end));
        // `code.bytes` is the source with comments and strings blanked.
        let mut declared = parameter_bounds(text.get(at + 1..close).unwrap_or_default());
        for (param, written) in where_bounds(text, close + 1, end) {
            declared.entry(param).or_default().extend(written);
        }
        for param in params {
            let written = declared.remove(&param).unwrap_or_default();
            bounds.entry(param.clone()).or_default().push(Declared {
                start: first,
                end: last,
                bounds: written,
            });
            found.entry(param).or_default().push((first, last));
        }
        i = close + 1;
    }
    let by_name = found
        .into_iter()
        .map(|(name, mut ranges)| {
            ranges.sort_unstable();
            let mut merged: Vec<(u32, u32)> = Vec::with_capacity(ranges.len());
            for (start, end) in ranges {
                match merged.last_mut() {
                    Some(last) if start <= last.1 => last.1 = last.1.max(end),
                    _ => merged.push((start, end)),
                }
            }
            (name, merged)
        })
        .collect();
    for declarations in bounds.values_mut() {
        declarations.sort_by_key(|declared| declared.start);
    }
    GenericScopes { by_name, bounds }
}

/// The bounds written inline in a generic list: `T: A + B<X>, U` ->
/// {`T`: [`A`, `B`]}.
fn parameter_bounds(list: &str) -> HashMap<String, Vec<String>> {
    split_top(list)
        .into_iter()
        .filter_map(|entry| {
            let (name, written) = entry.split_once(':')?;
            let name = name.trim();
            is_plain(name).then(|| (name.to_string(), bound_paths(written)))
        })
        .collect()
}

/// The `where` clauses between an item's generic list (`from`) and its body
/// or `;` (before `end`): (`T`, [bounds]) for each `T: …` clause.
fn where_bounds(text: &str, from: usize, end: usize) -> Vec<(String, Vec<String>)> {
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut clause_start = None;
    let mut at = from;
    let limit = end.min(bytes.len());
    while at < limit {
        match bytes[at] {
            b'(' | b'[' | b'<' => depth += 1,
            b'>' if at > 0 && bytes[at - 1] == b'-' => {}
            b')' | b']' | b'>' => depth -= 1,
            b'{' | b';' if depth <= 0 => break,
            b'w' if depth <= 0
                && clause_start.is_none()
                && bytes[at..].starts_with(b"where")
                && (at == 0 || !is_ident(bytes[at - 1]))
                && !bytes.get(at + 5).is_some_and(|&byte| is_ident(byte)) =>
            {
                clause_start = Some(at + 5);
                at += 5;
                continue;
            }
            _ => {}
        }
        at += 1;
    }
    let Some(start) = clause_start else {
        return Vec::new();
    };
    split_top(&text[start..at])
        .into_iter()
        .filter_map(|clause| {
            let clause = clause.trim();
            // `for<'a> F: Fn(&'a T)`: the binder names no parameter.
            let clause = match clause.strip_prefix("for") {
                Some(rest) if rest.trim_start().starts_with('<') => {
                    let rest = rest.trim_start();
                    let close = closing_angle(rest.as_bytes(), 0)?;
                    rest[close + 1..].trim_start()
                }
                _ => clause,
            };
            let (name, written) = clause.split_once(':')?;
            let name = name.trim();
            is_plain(name).then(|| (name.to_string(), bound_paths(written)))
        })
        .collect()
}

/// `A + ?Sized + B<X> + 'a + Fn(u8) -> T` -> [`A`, `B`, `Fn`]: each trait
/// bound's path, generic arguments and lifetimes dropped.
fn bound_paths(written: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    let bytes = written.as_bytes();
    for (index, &byte) in bytes.iter().enumerate() {
        match byte {
            b'<' | b'(' | b'[' => depth += 1,
            b'>' if index > 0 && bytes[index - 1] == b'-' => {}
            b'>' | b')' | b']' => depth -= 1,
            b'+' if depth == 0 => {
                parts.push(&written[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&written[start..]);
    parts
        .into_iter()
        .filter_map(|part| {
            let part = part.trim();
            let part = part.strip_prefix("~const ").unwrap_or(part);
            let part = part.strip_prefix("const ").unwrap_or(part).trim_start();
            if part.starts_with(['\'', '?']) {
                return None;
            }
            let end = part
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
                .unwrap_or(part.len());
            let path = part[..end].trim_end_matches(':');
            (!path.is_empty()).then(|| path.to_string())
        })
        .collect()
}

/// A plain identifier (a parameter name, not `'a`, `T::Item` or `const N`).
fn is_plain(name: &str) -> bool {
    name.bytes().next().is_some_and(is_ident_start) && name.bytes().all(is_ident)
}

/// The type and const parameters of a generic list (`'a, T: Bound<U>, const
/// N: usize` -> `T`, `N`); lifetimes name no type.
fn parameter_names(list: &str) -> Vec<String> {
    split_top(list)
        .into_iter()
        .filter_map(|entry| {
            let entry = entry.trim();
            let entry = entry.strip_prefix("const ").unwrap_or(entry).trim_start();
            if entry.starts_with('\'') {
                return None;
            }
            let end = entry
                .bytes()
                .position(|byte| !is_ident(byte))
                .unwrap_or(entry.len());
            (end > 0).then(|| entry[..end].to_string())
        })
        .collect()
}

/// `list` split at its top-level commas.
fn split_top(list: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    let bytes = list.as_bytes();
    for (index, &byte) in bytes.iter().enumerate() {
        match byte {
            b'<' | b'(' | b'[' => depth += 1,
            b'>' if index > 0 && bytes[index - 1] == b'-' => {}
            b'>' | b')' | b']' => depth -= 1,
            b',' if depth == 0 => {
                parts.push(&list[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&list[start..]);
    parts
}

/// The `>` closing the `<` at `open`, `->` arrows skipped.
fn closing_angle(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (index, &byte) in bytes.iter().enumerate().skip(open) {
        match byte {
            b'<' => depth += 1,
            b'>' if index > 0 && bytes[index - 1] == b'-' => {}
            b'>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            b'{' | b';' => return None,
            _ => {}
        }
    }
    None
}

/// Where the item whose generics end before `from` ends: its `;`, or the
/// `}` closing its body.
fn item_end(bytes: &[u8], from: usize) -> usize {
    let mut depth = 0i32;
    let mut body = false;
    for (index, &byte) in bytes.iter().enumerate().skip(from) {
        match byte {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b';' if depth <= 0 && !body => return index,
            b'{' => {
                body = true;
                depth += 1;
            }
            b'}' => {
                depth -= 1;
                if body && depth == 0 {
                    return index;
                }
            }
            _ => {}
        }
    }
    bytes.len().saturating_sub(1)
}

fn skip_space(bytes: &[u8], mut at: usize) -> usize {
    while at < bytes.len() && bytes[at].is_ascii_whitespace() {
        at += 1;
    }
    at
}

fn ident_end(bytes: &[u8], mut at: usize) -> usize {
    while at < bytes.len() && is_ident(bytes[at]) {
        at += 1;
    }
    at
}

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// A file's text with comments, strings and char literals blanked (same
/// length, newlines kept), and its line starts.
struct Code {
    bytes: Vec<u8>,
    line_starts: Vec<usize>,
}

impl Code {
    /// The blanked text. Blanking replaces whole literals and comments by
    /// spaces, so it stays UTF-8 wherever the source is ASCII; a non-ASCII
    /// byte left in code (an identifier) keeps its whole char.
    fn text(&self) -> &str {
        std::str::from_utf8(&self.bytes).unwrap_or_default()
    }

    fn of(source: &str) -> Code {
        let src = source.as_bytes();
        let mut bytes = src.to_vec();
        let blank = |bytes: &mut Vec<u8>, from: usize, to: usize| {
            for byte in &mut bytes[from..to.min(src.len())] {
                if *byte != b'\n' {
                    *byte = b' ';
                }
            }
        };
        let mut i = 0;
        while i < src.len() {
            let next = src.get(i + 1).copied();
            match src[i] {
                b'/' if next == Some(b'/') => {
                    let end = src[i..]
                        .iter()
                        .position(|&byte| byte == b'\n')
                        .map_or(src.len(), |at| i + at);
                    blank(&mut bytes, i, end);
                    i = end;
                }
                b'/' if next == Some(b'*') => {
                    let mut depth = 0;
                    let mut end = i;
                    while end < src.len() {
                        if src[end..].starts_with(b"/*") {
                            depth += 1;
                            end += 2;
                        } else if src[end..].starts_with(b"*/") {
                            depth -= 1;
                            end += 2;
                            if depth == 0 {
                                break;
                            }
                        } else {
                            end += 1;
                        }
                    }
                    blank(&mut bytes, i, end);
                    i = end;
                }
                b'r' if (next == Some(b'"') || next == Some(b'#'))
                    && (i == 0 || !is_ident(src[i - 1])) =>
                {
                    let hashes = src[i + 1..]
                        .iter()
                        .take_while(|&&byte| byte == b'#')
                        .count();
                    let open = i + 1 + hashes;
                    if src.get(open) != Some(&b'"') {
                        i += 1;
                        continue;
                    }
                    let mut closing = vec![b'"'];
                    closing.extend(std::iter::repeat_n(b'#', hashes));
                    let end = src[open + 1..]
                        .windows(closing.len())
                        .position(|window| window == closing.as_slice())
                        .map_or(src.len(), |at| open + 1 + at + closing.len());
                    blank(&mut bytes, i, end);
                    i = end;
                }
                b'"' => {
                    let mut end = i + 1;
                    while end < src.len() && src[end] != b'"' {
                        end += if src[end] == b'\\' { 2 } else { 1 };
                    }
                    let end = (end + 1).min(src.len());
                    blank(&mut bytes, i, end);
                    i = end;
                }
                b'\'' => {
                    // A char literal (`'x'`, `'\n'`, `'\u{1F600}'`), not a
                    // lifetime (`'a`).
                    let end = if next == Some(b'\\') {
                        src[i + 2..]
                            .iter()
                            .position(|&byte| byte == b'\'')
                            .map(|at| i + 2 + at + 1)
                    } else {
                        let width = source[i + 1..].chars().next().map_or(1, char::len_utf8);
                        (src.get(i + 1 + width) == Some(&b'\'')).then_some(i + 2 + width)
                    };
                    match end {
                        Some(end) => {
                            blank(&mut bytes, i, end);
                            i = end;
                        }
                        None => i += 1,
                    }
                }
                _ => i += 1,
            }
        }
        let line_starts = std::iter::once(0)
            .chain(
                src.iter()
                    .enumerate()
                    .filter(|&(_, &byte)| byte == b'\n')
                    .map(|(at, _)| at + 1),
            )
            .collect();
        Code { bytes, line_starts }
    }

    /// The 1-based line of byte `offset`.
    fn line_of(&self, offset: usize) -> u32 {
        let index = self.line_starts.partition_point(|&start| start <= offset);
        u32::try_from(index.max(1)).unwrap_or(u32::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scopes(source: &str) -> GenericScopes {
        scan(source)
    }

    #[test]
    fn reads_the_parameters_of_generic_items_and_their_lines() {
        let source = "\
struct R;
impl<R: Read, const N: usize> Buf<R> {
    fn fill<'a, W>(&'a mut self, w: W) -> R
    where
        W: Fn() -> R,
    {
        let c = '{';
        let s = \"}\";
        todo!()
    }
}
fn plain(r: R) {}
trait Tr<A> { fn get(&self) -> A; }
";
        let scopes = scopes(source);
        assert!(scopes.contains("R", 2));
        assert!(scopes.contains("R", 9));
        assert!(scopes.contains("N", 5));
        assert!(scopes.contains("W", 4));
        assert!(!scopes.contains("W", 11), "the fn's parameter ends with it");
        assert!(
            !scopes.contains("R", 12),
            "outside the impl `R` is the struct"
        );
        assert!(scopes.contains("A", 13));
        assert!(!scopes.contains("a", 3), "lifetimes name no type");
    }

    #[test]
    fn reads_each_parameters_bounds_inline_and_in_where_clauses() {
        let source = "\
impl<R: AsyncRead + ?Sized + 'static, W> Copy<R, W>
where
    W: crate::io::AsyncWrite + Unpin,
    for<'a> F: Fn(&'a R),
{
    fn run<R: Seek>(&self) {
        todo!()
    }
    fn other(&self) {}
}
fn plain<T>(t: T) {}
";
        let scopes = scopes(source);
        assert_eq!(scopes.bounds("R", 9), Some(&["AsyncRead".to_string()][..]));
        assert_eq!(
            scopes.bounds("W", 9),
            Some(&["crate::io::AsyncWrite".to_string(), "Unpin".to_string()][..])
        );
        // The inner `fn run<R: Seek>` shadows the impl's `R`.
        assert_eq!(scopes.bounds("R", 7), Some(&["Seek".to_string()][..]));
        assert_eq!(scopes.bounds("T", 11), Some(&[][..]));
        assert_eq!(scopes.bounds("T", 12), None);
    }

    #[test]
    fn reads_supertraits() {
        let found = scan_supertraits(
            "pub trait IntoUrl: IntoUrlSealed {}\n\
             pub(crate) trait Shape<T>: Named + fmt::Debug where T: Clone { fn a(&self); }\n\
             trait Plain { fn b(&self); }\n\
             // trait Commented: Nope {}\n",
        );
        assert_eq!(found["IntoUrl"], ["IntoUrlSealed"]);
        assert_eq!(found["Shape"], ["Named", "fmt::Debug"]);
        assert!(!found.contains_key("Plain"));
        assert!(!found.contains_key("Commented"));
    }

    #[test]
    fn a_declaration_without_a_body_ends_at_its_semicolon() {
        let source = "type Pair<T> = (T, T);\nfn f<U>();\nstruct T;\n";
        let scopes = scopes(source);
        assert!(scopes.contains("T", 1));
        assert!(!scopes.contains("T", 3));
        assert!(scopes.contains("U", 2));
    }
}

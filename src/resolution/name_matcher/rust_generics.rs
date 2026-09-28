//! The generic parameters in scope at a line of a Rust file.
//!
//! A bare `T` in a type names the enclosing item's parameter, never a
//! project type that happens to be called `T` (a test's `struct R;`, an
//! `enum E`): rustc resolves a generic parameter before any module item.
//! The index keeps no generics, so each file's items that declare them
//! (`fn f<T>`, `impl<R: Read>`, `struct S<W>`, `trait Tr<A>`, `enum`,
//! `union`, `type`) are read from its text once, with the lines each item
//! spans, and kept per file (with the source it was read from) on each
//! resolving thread.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::resolution::types::{ResolutionContext, UnresolvedRef};

/// Each generic parameter name of a file, with the disjoint line ranges
/// (1-based, inclusive) of the items declaring it.
#[derive(Debug, Default)]
struct GenericScopes {
    by_name: HashMap<String, Vec<(u32, u32)>>,
}

impl GenericScopes {
    fn contains(&self, name: &str, line: u32) -> bool {
        let Some(ranges) = self.by_name.get(name) else {
            return false;
        };
        let at = ranges.partition_point(|&(start, _)| start <= line);
        at > 0 && ranges[at - 1].1 >= line
    }
}

/// Whether `name` is a generic parameter of an item enclosing the
/// reference's line.
pub(super) fn is_generic_param(
    name: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> bool {
    thread_local! {
        /// Per file, with the source it was read from: references of every
        /// file of a project interleave, more than a few-file memo holds.
        static SCOPES: RefCell<HashMap<String, (Arc<str>, Arc<GenericScopes>)>> =
            RefCell::new(HashMap::new());
    }
    let Some(source) = context.read_file_arc(&reference.file_path) else {
        return false;
    };
    let cached = SCOPES.with(|memo| {
        memo.borrow()
            .get(&reference.file_path)
            .filter(|(seen, _)| Arc::ptr_eq(seen, &source))
            .map(|(_, scopes)| Arc::clone(scopes))
    });
    let scopes = cached.unwrap_or_else(|| {
        let scopes = Arc::new(scan(&source));
        SCOPES.with(|memo| {
            memo.borrow_mut().insert(
                reference.file_path.clone(),
                (Arc::clone(&source), Arc::clone(&scopes)),
            )
        });
        scopes
    });
    scopes.contains(name, reference.line)
}

/// Items that can declare generic parameters.
const GENERIC_ITEMS: &[&str] = &["enum", "fn", "impl", "struct", "trait", "type", "union"];

fn scan(source: &str) -> GenericScopes {
    let code = Code::of(source);
    let mut found: HashMap<String, Vec<(u32, u32)>> = HashMap::new();
    let bytes = code.bytes.as_slice();
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
        for param in params {
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
    GenericScopes { by_name }
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
    fn a_declaration_without_a_body_ends_at_its_semicolon() {
        let source = "type Pair<T> = (T, T);\nfn f<U>();\nstruct T;\n";
        let scopes = scopes(source);
        assert!(scopes.contains("T", 1));
        assert!(!scopes.contains("T", 3));
        assert!(scopes.contains("U", 2));
    }
}

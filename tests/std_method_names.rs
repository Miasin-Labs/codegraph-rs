//! Regenerates `src/resolution/name_matcher/std_method_names.rs`: every
//! public method name `core`, `alloc`, and `std` define on a type or trait,
//! read from the library source of the toolchain `rustc` runs.
//!
//! ```text
//! rustup component add rust-src
//! CODEGRAPH_REGENERATE_STD_METHODS=1 cargo test --test std_method_names -- --ignored
//! ```
//!
//! No network access and no build script: the scan reads the local
//! `$(rustc --print sysroot)/lib/rustlib/src/rust/library/{core,alloc,std}/src`
//! tree and rewrites the committed list. It is lexical, not a parse, because
//! much of std defines its methods inside `macro_rules!` bodies (every
//! integer method comes from `uint_impl!`/`int_impl!`), which a parser keeps
//! as opaque token trees. A name is kept when it is
//!
//! - any fn of a `pub trait` (`Iterator::next`, `Clone::clone`);
//! - a `pub fn` of an inherent `impl` block (`Vec::with_capacity`,
//!   `Path::parent`);
//! - a `pub fn` taking `self` anywhere else (a macro body that expands into an
//!   impl, such as `pub const fn count_ones(self)`).
//!
//! `#[doc(hidden)]` fns and the private `sys` implementation modules are
//! skipped.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const OUTPUT: &str = "src/resolution/name_matcher/std_method_names.rs";
const LIBRARY_CRATES: &[&str] = &["core", "alloc", "std"];
/// Directories whose `pub fn`s are tests or private implementation details.
const SKIPPED_DIRS: &[&str] = &["tests", "benches", "sys", "sys_common"];
const SKIPPED_FILES: &[&str] = &["tests.rs", "benches.rs"];

#[test]
#[ignore = "rewrites a committed source file from the toolchain's rust-src"]
fn regenerate_std_method_names() {
    if std::env::var_os("CODEGRAPH_REGENERATE_STD_METHODS").is_none() {
        eprintln!("set CODEGRAPH_REGENERATE_STD_METHODS=1 to rewrite {OUTPUT}");
        return;
    }
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let sysroot = run(&rustc, &["--print", "sysroot"]);
    let version = run(&rustc, &["--version"]);
    let library = Path::new(&sysroot).join("lib/rustlib/src/rust/library");
    assert!(
        library.is_dir(),
        "no library source at {}: run `rustup component add rust-src`",
        library.display()
    );
    let mut names = BTreeSet::new();
    for krate in LIBRARY_CRATES {
        for file in rust_files(&library.join(krate).join("src")) {
            let text = fs::read_to_string(&file)
                .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
            names.extend(method_names(&text));
        }
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(OUTPUT);
    fs::write(&path, render(&names, &version)).unwrap();
    eprintln!("wrote {} names to {}", names.len(), path.display());
}

/// The scanner on a few std shapes, so a regeneration can't silently drift.
#[test]
fn scanner_reads_std_method_shapes() {
    let source = r#"
        pub trait Iterator {
            type Item;
            fn next(&mut self) -> Option<Self::Item>;
            #[doc(hidden)]
            fn __hidden(&self) {}
            fn count(self) -> usize where Self: Sized { let _ = '{'; 0 }
        }
        pub(crate) trait Sealed { fn sealed_only(&self); }
        impl<T> Vec<T> {
            pub const fn with_capacity(capacity: usize) -> Self { todo!() }
            pub(crate) fn grow_internal(&mut self) {}
            fn private_helper(&self) {}
        }
        impl<T> Iterator for IntoIter<T> {
            fn next(&mut self) -> Option<T> { None }
            fn trait_impl_only(&self) {}
        }
        macro_rules! uint_impl {
            () => {
                /// Doc with a brace { in it.
                pub const fn count_ones(self) -> u32 { 0 }
                pub const fn from_str_radix(src: &str, radix: u32) -> Self { 0 }
            };
        }
        pub fn free_function(x: u8) -> impl Fn(u8) -> u8 { move |y| x + y }
        pub fn spawn<F: FnOnce() -> T, T>(f: F) -> JoinHandle<T> { let s = "}"; todo!() }
        impl<'a> Path<'a> {
            pub fn parent(&'a self) -> Option<&'a Path> { None }
        }
    "#;
    let names: BTreeSet<String> = method_names(source).into_iter().collect();
    let expected: BTreeSet<String> = ["count", "count_ones", "next", "parent", "with_capacity"]
        .into_iter()
        .map(String::from)
        .collect();
    assert_eq!(names, expected);
}

fn run(program: &str, args: &[&str]) -> String {
    let output = Command::new(program)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("run {program}: {error}"));
    assert!(output.status.success(), "{program} {args:?} failed");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// Every `.rs` file under `dir`, in a stable order.
fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if !SKIPPED_DIRS.contains(&name.as_str()) {
                    pending.push(path);
                }
            } else if name.ends_with(".rs") && !SKIPPED_FILES.contains(&name.as_str()) {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn render(names: &BTreeSet<String>, version: &str) -> String {
    let mut out = String::new();
    out.push_str("// @generated by tests/std_method_names.rs: do not edit by hand.\n");
    out.push_str("//! Every public method name `core`, `alloc`, and `std` define on a type or\n");
    out.push_str("//! trait (inherent `pub fn`s and trait items, `self` or not).\n");
    out.push_str("//!\n");
    out.push_str(&format!(
        "//! Generated from the library source of `{version}`.\n"
    ));
    out.push_str("//! Regenerate (needs the toolchain's `rust-src` component):\n");
    out.push_str("//!\n");
    out.push_str("//! ```text\n");
    out.push_str(
        "//! CODEGRAPH_REGENERATE_STD_METHODS=1 cargo test --test std_method_names -- --ignored\n",
    );
    out.push_str("//! ```\n");
    out.push('\n');
    out.push_str(&format!(
        "/// {} names, sorted for `binary_search`.\n",
        names.len()
    ));
    out.push_str("pub(super) const STD_METHOD_NAMES: &[&str] = &[\n");
    for name in names {
        out.push_str(&format!("    \"{name}\",\n"));
    }
    out.push_str("];\n");
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token<'a> {
    Ident(&'a str),
    Lifetime,
    Punct(char),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Frame {
    /// An `impl` block's body.
    Impl,
    /// A trait definition's body.
    Trait { public: bool },
    /// Any other `{…}`: a fn body, a module, a macro body.
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Visibility {
    Public,
    Restricted,
    Private,
}

/// The method names one library source file defines (see the module docs).
fn method_names(source: &str) -> Vec<String> {
    let stripped = strip_literals_and_comments(source);
    let tokens = tokenize(&stripped);
    let mut names = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    // The frame the next `{` opens, once an item header announced it.
    let mut pending: Option<Frame> = None;
    let mut hidden = false;
    let mut index = 0;
    while index < tokens.len() {
        match tokens[index] {
            Token::Punct('#') => {
                if let Some((end, is_hidden)) = attribute(&tokens, index) {
                    hidden |= is_hidden;
                    index = end;
                }
            }
            Token::Ident("impl") if at_item_start(&tokens, index) => pending = Some(Frame::Impl),
            Token::Ident("trait") => {
                pending = Some(Frame::Trait {
                    public: visibility(&tokens, index) == Visibility::Public,
                });
            }
            Token::Ident("fn") => {
                // `fn(u8) -> u8` is a fn-pointer type, not an item.
                if let Some(Token::Ident(name)) = tokens.get(index + 1) {
                    let public = visibility(&tokens, index) == Visibility::Public;
                    let keep = match stack.last() {
                        Some(Frame::Trait { public: true }) => true,
                        Some(Frame::Impl) => public,
                        _ => public && takes_self(&tokens, index + 2),
                    };
                    if keep && !hidden {
                        names.push((*name).to_string());
                    }
                    pending = Some(Frame::Other);
                    hidden = false;
                }
            }
            Token::Punct('{') => {
                stack.push(pending.take().unwrap_or(Frame::Other));
                hidden = false;
            }
            Token::Punct('}') => {
                stack.pop();
                pending = None;
                hidden = false;
            }
            Token::Punct(';') => {
                pending = None;
                hidden = false;
            }
            _ => {}
        }
        index += 1;
    }
    names
}

/// `impl` opens an item (not an `impl Trait` type) when it follows an item
/// boundary, an attribute, `unsafe`/`default`, or a macro repetition.
fn at_item_start(tokens: &[Token<'_>], index: usize) -> bool {
    match index.checked_sub(1).map(|previous| tokens[previous]) {
        None => true,
        Some(Token::Punct(punct)) => matches!(punct, '{' | '}' | ';' | ']' | '*'),
        Some(Token::Ident(word)) => matches!(word, "unsafe" | "default"),
        Some(Token::Lifetime) => false,
    }
}

/// The visibility written before the item keyword at `index`, past its
/// qualifiers (`pub const unsafe fn`, `pub unsafe auto trait`).
fn visibility(tokens: &[Token<'_>], index: usize) -> Visibility {
    let mut at = index;
    while at > 0 {
        match tokens[at - 1] {
            Token::Ident(
                "const" | "unsafe" | "async" | "extern" | "default" | "safe" | "auto" | "gen",
            ) => at -= 1,
            Token::Ident("pub") => return Visibility::Public,
            // `$vis fn` in a macro body: whatever the invocation passes.
            Token::Ident(_) if at >= 2 && tokens[at - 2] == Token::Punct('$') => {
                return Visibility::Public;
            }
            // `pub(crate) fn`, `pub(in path) fn`.
            Token::Punct(')') => return Visibility::Restricted,
            _ => return Visibility::Private,
        }
    }
    Visibility::Private
}

/// The fn whose generics or parameter list starts at `index` takes `self`
/// (`self`, `&self`, `&'a mut self`, `mut self`, `self: Box<Self>`).
fn takes_self(tokens: &[Token<'_>], mut index: usize) -> bool {
    if tokens.get(index) == Some(&Token::Punct('<')) {
        let mut depth = 0usize;
        while index < tokens.len() {
            match tokens[index] {
                Token::Punct('<') => depth += 1,
                // The `>` of `->` closes nothing.
                Token::Punct('>') if index > 0 && tokens[index - 1] != Token::Punct('-') => {
                    depth -= 1;
                    if depth == 0 {
                        index += 1;
                        break;
                    }
                }
                _ => {}
            }
            index += 1;
        }
    }
    if tokens.get(index) != Some(&Token::Punct('(')) {
        return false;
    }
    index += 1;
    while let Some(token) = tokens.get(index) {
        match token {
            Token::Punct('&') | Token::Lifetime | Token::Ident("mut") => index += 1,
            Token::Ident("self") => return true,
            _ => return false,
        }
    }
    false
}

/// `#[…]` or `#![…]` starting at `index`: the index of its `]` and whether it
/// is `#[doc(hidden)]`.
fn attribute(tokens: &[Token<'_>], index: usize) -> Option<(usize, bool)> {
    let mut open = index + 1;
    if tokens.get(open) == Some(&Token::Punct('!')) {
        open += 1;
    }
    if tokens.get(open) != Some(&Token::Punct('[')) {
        return None;
    }
    let mut depth = 0usize;
    for (at, token) in tokens.iter().enumerate().skip(open) {
        match token {
            Token::Punct('[') => depth += 1,
            Token::Punct(']') => {
                depth -= 1;
                if depth == 0 {
                    let body = &tokens[open + 1..at];
                    let hidden = body
                        == [
                            Token::Ident("doc"),
                            Token::Punct('('),
                            Token::Ident("hidden"),
                            Token::Punct(')'),
                        ];
                    return Some((at, hidden));
                }
            }
            _ => {}
        }
    }
    None
}

fn tokenize(text: &str) -> Vec<Token<'_>> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_whitespace() {
            index += 1;
        } else if byte == b'_' || byte.is_ascii_alphabetic() || byte >= 0x80 {
            let start = index;
            while index < bytes.len()
                && (bytes[index] == b'_'
                    || bytes[index].is_ascii_alphanumeric()
                    || bytes[index] >= 0x80)
            {
                index += 1;
            }
            tokens.push(Token::Ident(&text[start..index]));
        } else if byte.is_ascii_digit() {
            while index < bytes.len()
                && (bytes[index] == b'_' || bytes[index].is_ascii_alphanumeric())
            {
                index += 1;
            }
        } else if byte == b'\'' {
            index += 1;
            while index < bytes.len()
                && (bytes[index] == b'_' || bytes[index].is_ascii_alphanumeric())
            {
                index += 1;
            }
            tokens.push(Token::Lifetime);
        } else {
            tokens.push(Token::Punct(byte as char));
            index += 1;
        }
    }
    tokens
}

/// `source` with comments and string/char literals replaced by spaces, so
/// the braces and keywords inside them are not seen.
fn strip_literals_and_comments(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut index = 0;
    let blank = |out: &mut String, from: &[char]| {
        out.extend(from.iter().map(|c| if *c == '\n' { '\n' } else { ' ' }));
    };
    while index < chars.len() {
        let c = chars[index];
        let next = chars.get(index + 1).copied();
        let prev_ident =
            index > 0 && (chars[index - 1].is_alphanumeric() || chars[index - 1] == '_');
        let end = if c == '/' && next == Some('/') {
            let mut end = index;
            while end < chars.len() && chars[end] != '\n' {
                end += 1;
            }
            end
        } else if c == '/' && next == Some('*') {
            block_comment_end(&chars, index)
        } else if c == '"' {
            string_end(&chars, index + 1)
        } else if !prev_ident && matches!(c, 'r' | 'b' | 'c') {
            match literal_prefix_end(&chars, index) {
                Some(end) => end,
                None => {
                    out.push(c);
                    index += 1;
                    continue;
                }
            }
        } else if c == '\'' {
            match char_literal_end(&chars, index) {
                Some(end) => end,
                None => {
                    out.push(c);
                    index += 1;
                    continue;
                }
            }
        } else {
            out.push(c);
            index += 1;
            continue;
        };
        let end = end.min(chars.len());
        blank(&mut out, &chars[index..end]);
        index = end;
    }
    out
}

/// The index past a `/* … */` comment (nested ones included).
fn block_comment_end(chars: &[char], start: usize) -> usize {
    let mut depth = 0usize;
    let mut index = start;
    while index + 1 < chars.len() {
        match (chars[index], chars[index + 1]) {
            ('/', '*') => {
                depth += 1;
                index += 2;
            }
            ('*', '/') => {
                depth -= 1;
                index += 2;
                if depth == 0 {
                    return index;
                }
            }
            _ => index += 1,
        }
    }
    chars.len()
}

/// The index past the `"` closing a string whose body starts at `index`.
fn string_end(chars: &[char], mut index: usize) -> usize {
    while index < chars.len() {
        match chars[index] {
            '\\' => index += 2,
            '"' => return index + 1,
            _ => index += 1,
        }
    }
    chars.len()
}

/// `b"…"`, `c"…"`, `r#"…"#`, `br"…"`, `b'x'` starting at `start`: the index
/// past the literal, or `None` when `start` begins an identifier.
fn literal_prefix_end(chars: &[char], start: usize) -> Option<usize> {
    let mut index = start;
    if matches!(chars[index], 'b' | 'c') {
        index += 1;
        match chars.get(index) {
            Some('"') => return Some(string_end(chars, index + 1)),
            Some('\'') if chars[start] == 'b' => return char_literal_end(chars, index),
            Some('r') => {}
            _ => return None,
        }
    }
    // A raw string: `r`, some `#`s, then `"`.
    index += 1;
    let mut hashes = 0;
    while chars.get(index) == Some(&'#') {
        hashes += 1;
        index += 1;
    }
    if chars.get(index) != Some(&'"') {
        return None;
    }
    index += 1;
    while index < chars.len() {
        if chars[index] == '"'
            && chars[index + 1..]
                .iter()
                .take(hashes)
                .filter(|c| **c == '#')
                .count()
                == hashes
        {
            return Some(index + 1 + hashes);
        }
        index += 1;
    }
    Some(chars.len())
}

/// A char literal at `start` (`'x'`, `'\n'`, `'\u{1F600}'`): the index past
/// its closing quote. `None` for a lifetime or label (`'a`, `'outer:`).
fn char_literal_end(chars: &[char], start: usize) -> Option<usize> {
    match chars.get(start + 1)? {
        '\\' => {
            // Past the escaped char, so `'\''` ends at its second quote.
            let mut index = start + 3;
            while index < chars.len() && chars[index] != '\'' {
                index += 1;
            }
            Some(index + 1)
        }
        _ if chars.get(start + 2) == Some(&'\'') => Some(start + 3),
        _ => None,
    }
}

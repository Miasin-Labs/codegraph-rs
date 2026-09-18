//! Read the names a Rust `use` declaration binds.
//!
//! The index stores each `use` as one Import node: `name` is the first path
//! segment and `signature` is the declaration's source text, visibility and
//! brace lists included (`pub(in crate::mcp) use budget::{now_ms, cap as c};`).
//! [`parse_use_leaves`] flattens that text into one [`UseLeaf`] per bound
//! name — the shape rustc's `build_reduced_graph` gives a use tree before
//! import resolution walks it.

use crate::types::{Language, Node, NodeKind};

/// Who may name a binding: the `pub(…)` prefix of the declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UseVisibility {
    /// No prefix, or `pub(self)`: the declaring module and its descendants.
    Private,
    /// `pub`.
    Public,
    /// `pub(crate)`.
    Crate,
    /// `pub(super)`: the parent of the declaring module and its descendants.
    Super,
    /// `pub(in path)`: `path` and its descendants, as written.
    Restricted(Vec<String>),
}

/// What one leaf of a use tree binds in the declaring module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UseBinding {
    /// `a::b` or `a::b as c`: the name `b` (or `c`) in every namespace `b`
    /// occupies.
    Name(String),
    /// `a::{self}` or `a::{self as c}`: the module `a` alone.
    Module(String),
    /// `a::*`: every name the module exposes to the importer.
    Glob,
}

/// One bound name of a use tree, with its full path as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UseLeaf {
    pub vis: UseVisibility,
    /// The imported entity's path (`a::b` for `use a::{b as c}`); a glob's
    /// path is the module it reads from, and `{self}` maps to its parent.
    pub path: Vec<String>,
    pub binding: UseBinding,
}

impl UseLeaf {
    /// The name this leaf introduces, unless it is a glob.
    pub fn bound_name(&self) -> Option<&str> {
        match &self.binding {
            UseBinding::Name(name) | UseBinding::Module(name) => Some(name),
            UseBinding::Glob => None,
        }
    }
}

/// A use leaf together with where its declaration sits inside the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustUse {
    /// Inline `mod` blocks around the declaration (`mod tests { use … }`).
    pub inline_modules: Vec<String>,
    pub leaf: UseLeaf,
}

/// How many lines one fn-local `use` declaration may span.
const MAX_USE_LINES: usize = 20;

/// The use leaves of the `use` declarations written inside fn bodies of
/// `source` (indented `use` lines), which the index keeps no Import node
/// for.
pub fn rust_fn_local_uses(source: &str) -> Vec<UseLeaf> {
    let lines: Vec<&str> = source.lines().collect();
    let mut leaves = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.len() == line.len() || !trimmed.starts_with("use ") {
            continue;
        }
        let mut declaration = (*line).to_string();
        for next in lines.iter().skip(index + 1).take(MAX_USE_LINES) {
            if declaration.contains(';') {
                break;
            }
            declaration.push(' ');
            declaration.push_str(next);
        }
        leaves.extend(parse_use_leaves(&declaration));
    }
    leaves
}

/// Every use leaf declared by a file's Rust Import nodes.
pub fn rust_use_leaves<'a>(nodes: impl IntoIterator<Item = &'a Node>) -> Vec<RustUse> {
    nodes
        .into_iter()
        .filter(|node| node.language == Language::Rust && node.kind == NodeKind::Import)
        .flat_map(|node| {
            let inline = super::layout::inline_modules(&node.qualified_name);
            let leaves = node
                .signature
                .as_deref()
                .map(parse_use_leaves)
                .unwrap_or_default();
            leaves.into_iter().map(move |leaf| RustUse {
                inline_modules: inline.clone(),
                leaf,
            })
        })
        .collect()
}

/// Flatten a `use` declaration's text into the names it binds.
///
/// Comments and the trailing `;` are ignored, `{…}` lists expand against
/// their prefix, `self` in a list names the prefix module, and `as _` binds
/// nothing. `extern crate` and paths rooted at `::` name other crates, so
/// they yield no leaves. Malformed text yields whatever parsed cleanly.
pub fn parse_use_leaves(signature: &str) -> Vec<UseLeaf> {
    let tokens = tokenize(signature);
    let mut cursor = Cursor {
        tokens: &tokens,
        pos: 0,
    };
    let vis = cursor.visibility();
    let mut leaves = Vec::new();
    if cursor.eat_word("use") {
        cursor.tree(&vis, &mut Vec::new(), false, &mut leaves);
    }
    leaves
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token<'a> {
    Word(&'a str),
    PathSep,
    OpenBrace,
    CloseBrace,
    OpenParen,
    CloseParen,
    Comma,
    Star,
}

fn is_word_byte(byte: u8) -> bool {
    // Non-ASCII bytes belong to Unicode identifiers; a run of them always
    // ends on an ASCII byte, so the slice stays on a char boundary.
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'#') || !byte.is_ascii()
}

fn tokenize(text: &str) -> Vec<Token<'_>> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while let Some(&byte) = bytes.get(i) {
        let next = bytes.get(i + 1).copied();
        match (byte, next) {
            (b'/', Some(b'/')) => {
                while bytes.get(i).is_some_and(|&b| b != b'\n') {
                    i += 1;
                }
            }
            (b'/', Some(b'*')) => i = skip_block_comment(bytes, i),
            (b':', Some(b':')) => {
                tokens.push(Token::PathSep);
                i += 2;
            }
            _ if is_word_byte(byte) => {
                let start = i;
                while bytes.get(i).is_some_and(|&b| is_word_byte(b)) {
                    i += 1;
                }
                let word = &text[start..i];
                tokens.push(Token::Word(word.strip_prefix("r#").unwrap_or(word)));
            }
            _ => {
                let token = match byte {
                    b'{' => Some(Token::OpenBrace),
                    b'}' => Some(Token::CloseBrace),
                    b'(' => Some(Token::OpenParen),
                    b')' => Some(Token::CloseParen),
                    b',' => Some(Token::Comma),
                    b'*' => Some(Token::Star),
                    _ => None,
                };
                tokens.extend(token);
                i += 1;
            }
        }
    }
    tokens
}

/// Skip a (nestable) `/* … */` comment starting at `start`.
fn skip_block_comment(bytes: &[u8], start: usize) -> usize {
    let mut depth = 0usize;
    let mut i = start;
    while i < bytes.len() {
        match (bytes[i], bytes.get(i + 1).copied()) {
            (b'/', Some(b'*')) => {
                depth += 1;
                i += 2;
            }
            (b'*', Some(b'/')) => {
                depth -= 1;
                i += 2;
                if depth == 0 {
                    return i;
                }
            }
            _ => i += 1,
        }
    }
    i
}

struct Cursor<'t, 'a> {
    tokens: &'t [Token<'a>],
    pos: usize,
}

impl<'a> Cursor<'_, 'a> {
    fn peek(&self) -> Option<Token<'a>> {
        self.tokens.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<Token<'a>> {
        let token = self.peek();
        self.pos += usize::from(token.is_some());
        token
    }

    fn eat(&mut self, token: Token<'_>) -> bool {
        let matched = self.peek() == Some(token);
        self.pos += usize::from(matched);
        matched
    }

    fn eat_word(&mut self, word: &str) -> bool {
        self.eat(Token::Word(word))
    }

    fn visibility(&mut self) -> UseVisibility {
        if !self.eat_word("pub") {
            return UseVisibility::Private;
        }
        if !self.eat(Token::OpenParen) {
            return UseVisibility::Public;
        }
        let vis = match self.bump() {
            Some(Token::Word("crate")) => UseVisibility::Crate,
            Some(Token::Word("super")) => UseVisibility::Super,
            Some(Token::Word("self")) => UseVisibility::Private,
            Some(Token::Word("in")) => {
                let mut path = Vec::new();
                while let Some(token) = self.peek() {
                    match token {
                        Token::Word(segment) => path.push(segment.to_string()),
                        Token::PathSep => {}
                        _ => break,
                    }
                    self.pos += 1;
                }
                UseVisibility::Restricted(path)
            }
            _ => UseVisibility::Private,
        };
        self.eat(Token::CloseParen);
        vis
    }

    /// One use tree below `path` (the prefix so far), which it leaves as it
    /// found it. Nesting depth is bounded only by the input, so the
    /// recursion runs under the stack guard.
    fn tree(
        &mut self,
        vis: &UseVisibility,
        path: &mut Vec<String>,
        external: bool,
        out: &mut Vec<UseLeaf>,
    ) {
        crate::ensure_sufficient_stack(|| {
            let prefix_len = path.len();
            self.tree_inner(vis, path, external, out);
            path.truncate(prefix_len);
        });
    }

    fn tree_inner(
        &mut self,
        vis: &UseVisibility,
        path: &mut Vec<String>,
        external: bool,
        out: &mut Vec<UseLeaf>,
    ) {
        // `::std::x` (and `{::a, ::b}`) name another crate by its root.
        let rooted = self.eat(Token::PathSep);
        let external = external || (rooted && path.is_empty());
        loop {
            match self.bump() {
                Some(Token::Word(segment)) => {
                    path.push(segment.to_string());
                    if self.eat(Token::PathSep) {
                        continue;
                    }
                    let rename = if self.eat_word("as") {
                        match self.bump() {
                            Some(Token::Word(name)) => Some(name),
                            _ => return,
                        }
                    } else {
                        None
                    };
                    if !external {
                        out.extend(simple_leaf(vis, path.clone(), rename));
                    }
                    return;
                }
                Some(Token::Star) => {
                    if !external {
                        out.push(UseLeaf {
                            vis: vis.clone(),
                            path: path.clone(),
                            binding: UseBinding::Glob,
                        });
                    }
                    return;
                }
                Some(Token::OpenBrace) => {
                    while !matches!(self.peek(), None | Some(Token::CloseBrace)) {
                        self.tree(vis, path, external, out);
                        if !self.eat(Token::Comma) {
                            break;
                        }
                    }
                    self.eat(Token::CloseBrace);
                    return;
                }
                _ => return,
            }
        }
    }
}

/// The leaf for a non-glob path, or none for `as _` and bare keywords.
fn simple_leaf(
    vis: &UseVisibility,
    mut path: Vec<String>,
    rename: Option<&str>,
) -> Option<UseLeaf> {
    if rename == Some("_") {
        return None;
    }
    let binding = if path.last().is_some_and(|last| last == "self") {
        // `a::{self}` imports the module `a` itself.
        path.pop();
        let module = path.last()?;
        UseBinding::Module(rename.unwrap_or(module).to_string())
    } else {
        let last = path.last()?;
        if matches!(last.as_str(), "crate" | "super" | "$crate") && rename.is_none() {
            return None;
        }
        UseBinding::Name(rename.unwrap_or(last).to_string())
    };
    Some(UseLeaf {
        vis: vis.clone(),
        path,
        binding,
    })
}

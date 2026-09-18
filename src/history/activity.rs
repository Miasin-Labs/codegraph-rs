//! What a tool call did to a repository, as the cross-session memory records
//! it: the files it read, edited or searched, the identifiers it looked for,
//! and the build/test/commit outcome it produced.
//!
//! [`Activity::of`] redacts every raw string before looking at it (the same
//! redactor as [`super::ToolEvent::from_raw`]). What it returns is still not
//! a row: paths are absolute (the writer makes them repo-relative),
//! identifiers are resolved against the index or hashed by the writer, and
//! outcomes carry only a masked command template, error codes and a hash.

mod outcome;
mod shell;

use std::path::{Path, PathBuf};

pub(crate) use outcome::{Outcome, OutcomeKind, keeps_excerpt};

use super::event::RawToolCall;
use super::redact::redact;
use super::repo::absolutize;

/// Most identifiers kept from one call.
const MAX_IDENTS: usize = 12;
/// Most files kept from one call.
const MAX_TOUCHES: usize = 40;

/// How a call touched a file, strongest first (an edit beats a read beats a
/// search of the same file).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Op {
    Edit,
    Read,
    Search,
}

impl Op {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Read => "r",
            Self::Edit => "e",
            Self::Search => "s",
        }
    }

    pub(crate) fn from_str(s: &str) -> Option<Self> {
        match s {
            "r" => Some(Self::Read),
            "e" => Some(Self::Edit),
            "s" => Some(Self::Search),
            _ => None,
        }
    }
}

/// One file a call touched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Touch {
    /// Absolute, lexically normalized, redacted.
    pub path: PathBuf,
    pub op: Op,
    /// First line read, when the call named one.
    pub line: Option<u32>,
}

/// What one call did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Activity {
    pub touches: Vec<Touch>,
    /// Identifier-shaped names the call searched for (redacted text).
    pub idents: Vec<String>,
    pub outcome: Option<Outcome>,
}

/// The kind of tool, from its (agent-specific) name.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ToolClass {
    Read,
    Edit,
    Grep,
    Glob,
    Shell,
    /// A codegraph tool, by short name (`explore`, `node`, …).
    Codegraph(String),
    Other,
}

fn tool_class(tool: &str) -> ToolClass {
    let lower = tool.to_ascii_lowercase();
    if let Some(at) = lower.rfind("codegraph_") {
        return ToolClass::Codegraph(lower[at + "codegraph_".len()..].to_owned());
    }
    match lower.as_str() {
        "read" | "view" | "notebookread" | "look_at" | "read_file" | "readfile" => ToolClass::Read,
        "edit" | "multiedit" | "write" | "notebookedit" | "apply_patch" | "patch" | "edit_file"
        | "write_file" | "create_file" | "str_replace_editor" => ToolClass::Edit,
        "grep" | "search" | "rg" | "ripgrep" | "search_text" => ToolClass::Grep,
        "glob" | "find" | "list" | "ls" => ToolClass::Glob,
        "bash" | "shell" | "interactive_bash" | "exec" | "run_command" | "execute_command"
        | "monitor" | "terminal" => ToolClass::Shell,
        _ => ToolClass::Other,
    }
}

impl Activity {
    /// Derive what `raw` did. Every raw string is redacted first.
    pub(crate) fn of(raw: &RawToolCall) -> Self {
        let clean = |s: &Option<String>| s.as_deref().map(|s| redact(s).0);
        let cwd = clean(&raw.cwd);
        let cwd = cwd.as_deref().map(Path::new);
        let mut act = Self::default();
        let file = clean(&raw.file_path);
        match tool_class(&raw.tool) {
            ToolClass::Read => act.touch(file.as_deref(), cwd, Op::Read, raw.line),
            ToolClass::Edit => {
                act.touch(file.as_deref(), cwd, Op::Edit, None);
                for extra in &raw.extra_paths {
                    act.touch(Some(&redact(extra).0), cwd, Op::Edit, None);
                }
            }
            ToolClass::Grep => {
                if let Some(pattern) = clean(&raw.pattern) {
                    act.idents.extend(idents_of_pattern(&pattern));
                }
                if let Some(path) = clean(&raw.search_path).filter(|p| has_extension(p)) {
                    act.touch(Some(&path), cwd, Op::Search, None);
                }
            }
            ToolClass::Glob => {}
            ToolClass::Shell => {
                if let Some(command) = clean(&raw.command) {
                    shell::analyze(&command, cwd, raw.result.as_ref(), &mut act);
                }
            }
            ToolClass::Codegraph(short) => {
                for symbol in &raw.symbols {
                    act.idents.extend(idents_of_symbol(&redact(symbol).0));
                }
                if let Some(query) = clean(&raw.query) {
                    act.idents.extend(idents_of_query(&query));
                }
                if short == "node" || short == "explore" {
                    act.touch(file.as_deref(), cwd, Op::Read, raw.line);
                }
            }
            ToolClass::Other => {}
        }
        act.finish()
    }

    fn touch(&mut self, path: Option<&str>, cwd: Option<&Path>, op: Op, line: Option<u32>) {
        let Some(path) = path.map(strip_line_suffix) else {
            return;
        };
        if path.contains("://") || path.contains(['*', '$', '\n']) {
            return;
        }
        if let Some(path) = absolutize(path, cwd) {
            if !path.starts_with("/dev") && !path.starts_with("/proc") && !path.starts_with("/sys")
            {
                self.touches.push(Touch { path, op, line });
            }
        }
    }

    /// Dedupe touches (an edit beats a read beats a search of the same
    /// file) and identifiers, and cap both.
    fn finish(mut self) -> Self {
        let mut touches: Vec<Touch> = Vec::with_capacity(self.touches.len());
        for t in self.touches.drain(..) {
            match touches.iter_mut().find(|k| k.path == t.path) {
                Some(kept) if t.op < kept.op => *kept = t,
                Some(_) => {}
                None => touches.push(t),
            }
        }
        touches.truncate(MAX_TOUCHES);
        self.touches = touches;
        let mut seen = std::collections::HashSet::new();
        self.idents.retain(|i| seen.insert(i.clone()));
        self.idents.truncate(MAX_IDENTS);
        self
    }
}

/// `src/a.rs:10` / `src/a.rs:10:5` → `src/a.rs`.
fn strip_line_suffix(path: &str) -> &str {
    let mut p = path.trim().trim_matches(['\'', '"']);
    for _ in 0..2 {
        match p.rsplit_once(':') {
            Some((head, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => {
                p = head;
            }
            _ => break,
        }
    }
    p
}

/// A file-ish token: has a `/`, or ends in a short extension.
pub(crate) fn is_pathish(tok: &str) -> bool {
    if tok.is_empty() || tok.starts_with('-') || tok.contains("://") {
        return false;
    }
    let ok_chars = tok.chars().all(|c| {
        c.is_alphanumeric()
            || matches!(c, '_' | '.' | '@' | '+' | '/' | '-' | ',' | ':' | '=' | '~')
    });
    if !ok_chars || tok.bytes().all(|b| b.is_ascii_digit() || b == b',') {
        return false;
    }
    tok.contains('/') || has_extension(tok)
}

/// Ends in `.<letter><up to 7 alnum>` (`a.rs`, `Cargo.toml`).
pub(crate) fn has_extension(tok: &str) -> bool {
    let tok = strip_line_suffix(tok);
    let Some((stem, ext)) = tok.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && !stem.ends_with('/')
        && (1..=8).contains(&ext.len())
        && ext.starts_with(|c: char| c.is_ascii_alphabetic())
        && ext.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Common words and keywords that are never the thing being looked for.
const STOP: &[&str] = &[
    "fn",
    "pub",
    "struct",
    "impl",
    "enum",
    "trait",
    "mod",
    "use",
    "let",
    "mut",
    "const",
    "static",
    "def",
    "class",
    "function",
    "return",
    "self",
    "Self",
    "import",
    "from",
    "async",
    "await",
    "type",
    "interface",
    "export",
    "var",
    "new",
    "this",
    "true",
    "false",
    "none",
    "None",
    "null",
    "todo",
    "TODO",
    "fixme",
    "FIXME",
    "error",
    "err",
    "test",
    "tests",
    "the",
    "and",
    "for",
    "not",
    "with",
    "into",
    "while",
    "match",
    "loop",
    "where",
    "crate",
    "super",
    "dyn",
    "ref",
    "move",
    "unsafe",
    "extern",
    "else",
    "elif",
    "then",
    "done",
    "echo",
    "print",
    "println",
    "printf",
    "string",
    "String",
    "str",
    "int",
    "bool",
    "usize",
    "u32",
    "u64",
    "i32",
    "i64",
    "f32",
    "f64",
    "Vec",
    "Option",
    "Result",
    "Some",
    "Ok",
    "Err",
    "Box",
    "public",
    "private",
    "protected",
    "void",
    "package",
    "include",
    "define",
    "ifdef",
    "endif",
    "main",
    "args",
    "argv",
    "warning",
    "warn",
    "info",
    "debug",
    "trace",
    "log",
    "get",
    "set",
    "add",
    "remove",
    "list",
    "map",
    "value",
    "key",
    "name",
    "data",
    "file",
    "path",
    "line",
    "lines",
    "src",
    "lib",
    "bin",
    "tmp",
    "home",
    "all",
    "any",
    "has",
    "can",
    "will",
    "should",
    "how",
    "what",
    "does",
    "work",
    "works",
];

fn is_stop(word: &str) -> bool {
    STOP.contains(&word) || STOP.iter().any(|s| s.eq_ignore_ascii_case(word))
}

/// Identifier runs (`[A-Za-z_][A-Za-z0-9_]{2,}`) in `text`.
fn identifier_runs(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| w.len() >= 3 && w.len() <= 80)
        .filter(|w| w.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_'))
}

/// Identifiers a grep pattern looks for: regex syntax dropped, then the
/// identifier-shaped words that aren't common words. A redaction mask
/// (`<REDACTED>`) is never an identifier.
pub(crate) fn idents_of_pattern(pattern: &str) -> Vec<String> {
    if pattern.len() > 400 || pattern.contains("<REDACTED") {
        return Vec::new();
    }
    // `\bfoo\b`, `\w+`, `\s*` are regex escapes, not letters.
    let mut plain = String::with_capacity(pattern.len());
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            chars.next();
            plain.push(' ');
        } else {
            plain.push(c);
        }
    }
    identifier_runs(&plain)
        .filter(|w| !is_stop(w))
        .map(str::to_owned)
        .take(MAX_IDENTS)
        .collect()
}

/// Identifiers in a symbol argument (`Foo::bar`, `a.b`, `x, y`).
pub(crate) fn idents_of_symbol(symbol: &str) -> Vec<String> {
    if symbol.contains("<REDACTED") {
        return Vec::new();
    }
    identifier_runs(symbol)
        .filter(|w| !is_stop(w))
        .map(str::to_owned)
        .collect()
}

/// Code-shaped words of a free-text query: `snake_case`, `camelCase`,
/// `PascalCase` with an inner capital, `a::b` paths. Plain English is skipped.
pub(crate) fn idents_of_query(query: &str) -> Vec<String> {
    if query.contains("<REDACTED") {
        return Vec::new();
    }
    let mut out = Vec::new();
    for token in
        query.split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '|' | '(' | ')'))
    {
        let token = token.trim_matches(|c: char| matches!(c, '`' | '\'' | '"' | '.' | '?'));
        let codey = token.contains('_')
            || token.contains("::")
            || token
                .as_bytes()
                .windows(2)
                .any(|w| w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase());
        if codey {
            out.extend(
                identifier_runs(token)
                    .filter(|w| !is_stop(w))
                    .map(str::to_owned),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests;

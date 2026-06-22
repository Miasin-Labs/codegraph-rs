use regex::Regex;

use super::super::support::{
    angle_re,
    cpp_keyword_re,
    cpp_source_ext_re,
    ptr_ref_re,
    whitespace_re,
};
use crate::resolution::types::{ResolutionContext, UnresolvedRef};

/// C++ keywords/control-flow tokens that can appear right before a receiver
/// (e.g. `return ptr->m()`) and must NOT be treated as a type.
const CPP_NON_TYPE_TOKENS: [&str; 29] = [
    "return",
    "if",
    "else",
    "for",
    "while",
    "do",
    "switch",
    "case",
    "default",
    "break",
    "continue",
    "goto",
    "throw",
    "new",
    "delete",
    "co_await",
    "co_yield",
    "co_return",
    "static_cast",
    "const_cast",
    "dynamic_cast",
    "reinterpret_cast",
    "sizeof",
    "alignof",
    "typeid",
    "and",
    "or",
    "not",
    "xor",
];

fn is_cpp_non_type_token(token: &str) -> bool {
    CPP_NON_TYPE_TOKENS.contains(&token)
}

pub(in crate::resolution::name_matcher) fn normalize_cpp_type_name(
    type_name: &str,
) -> Option<String> {
    let s = cpp_keyword_re().replace_all(type_name, " ");
    let s = ptr_ref_re().replace_all(&s, " ");
    let s = angle_re().replace_all(&s, " ");
    let s = whitespace_re().replace_all(&s, " ");
    let normalized = s.trim();

    if normalized.is_empty() {
        return None;
    }
    let parts: Vec<&str> = normalized.split("::").filter(|p| !p.is_empty()).collect();
    let last = *parts.last()?;
    if last.is_empty() {
        return None;
    }
    if is_cpp_non_type_token(last) {
        return None;
    }
    Some(last.to_string())
}

/// Declarator regex: matches `Type receiver`, `Type* receiver`, `Type *receiver`,
/// `Type*receiver`, `Type<X> receiver`, etc., REQUIRING a declarator terminator
/// (`;`, `=`, `,`, `)`, `[`, `{`, `(`, or end-of-line) after the receiver. The
/// terminator rules out uses like `return receiver->m()` where the preceding
/// token is a keyword, not a type.
///
/// Deviation: the TS regex used a lookahead `(?=[;=,)\[{(]|$)` which the
/// `regex` crate doesn't support; since only the FIRST match's capture group 1
/// is consumed, a consuming non-capturing group is observably equivalent
/// (leftmost-first start position and group-1 contents are identical).
fn build_declarator_regex(escaped_receiver: &str) -> Regex {
    Regex::new(&format!(
        r"([A-Za-z_][0-9A-Za-z_:]*(?:\s*<[^;=(){{}}]+>)?(?:\s*[*&]+)?)\s*\b{escaped_receiver}\b\s*(?:[;=,)\[{{(]|$)"
    ))
    .expect("valid declarator regex")
}

/// Split source into lines like JS `split(/\r?\n/)` (a lone `\r` is NOT a
/// separator; a trailing `\r` before `\n` is stripped).
fn split_lines(source: &str) -> Vec<&str> {
    source
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect()
}

pub(in crate::resolution::name_matcher) fn infer_cpp_receiver_type(
    receiver_name: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let source = context.read_file(&reference.file_path)?;
    if source.is_empty() {
        return None;
    }

    let lines = split_lines(&source);
    let call_line_index = ((reference.line as i64) - 1).clamp(0, lines.len() as i64 - 1) as usize;

    // Receiver names repeat constantly across a codebase's references
    // (`this`, `ctx`, `builder`, ...), and `Regex::new` costs tens of µs —
    // compiling two regexes PER REFERENCE was ~10% of the entire llvm
    // resolution pass. Memoize per receiver in a thread-local map (bounded:
    // distinct receiver identifiers are vocabulary-sized, not ref-sized).
    thread_local! {
        static RECEIVER_REGEX_CACHE: std::cell::RefCell<std::collections::HashMap<String, (Regex, Regex)>> =
            std::cell::RefCell::new(std::collections::HashMap::new());
    }
    let (receiver_pattern, declarator_regex) = RECEIVER_REGEX_CACHE.with(|cache| {
        if let Some(pair) = cache.borrow().get(receiver_name) {
            return pair.clone();
        }
        let escaped_receiver = regex::escape(receiver_name);
        let pair = (
            Regex::new(&format!(r"\b{escaped_receiver}\b")).expect("valid receiver regex"),
            build_declarator_regex(&escaped_receiver),
        );
        let mut map = cache.borrow_mut();
        // Safety valve: a pathological generated file with millions of
        // distinct receivers must not grow the cache unboundedly.
        if map.len() >= 65536 {
            map.clear();
        }
        map.insert(receiver_name.to_string(), pair.clone());
        pair
    });

    for i in (0..=call_line_index).rev() {
        let line = lines[i];
        if line.is_empty() || !receiver_pattern.is_match(line) {
            continue;
        }

        if let Some(caps) = declarator_regex.captures(line) {
            let type_text = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            if let Some(normalized) = normalize_cpp_type_name(type_text) {
                return Some(normalized);
            }
        }
    }

    let raw_candidates = [
        cpp_source_ext_re()
            .replace(&reference.file_path, ".h")
            .into_owned(),
        cpp_source_ext_re()
            .replace(&reference.file_path, ".hpp")
            .into_owned(),
        cpp_source_ext_re()
            .replace(&reference.file_path, ".hxx")
            .into_owned(),
    ];
    let mut header_candidates: Vec<&String> = Vec::new();
    for candidate in &raw_candidates {
        if !header_candidates.contains(&candidate) && candidate != &reference.file_path {
            header_candidates.push(candidate);
        }
    }

    for header_path in header_candidates {
        if !context.file_exists(header_path) {
            continue;
        }
        let Some(header_source) = context.read_file(header_path) else {
            continue;
        };
        if header_source.is_empty() {
            continue;
        }

        for line in split_lines(&header_source) {
            if !receiver_pattern.is_match(line) {
                continue;
            }
            let Some(caps) = declarator_regex.captures(line) else {
                continue;
            };
            let type_text = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            if let Some(normalized) = normalize_cpp_type_name(type_text) {
                return Some(normalized);
            }
        }
    }

    None
}

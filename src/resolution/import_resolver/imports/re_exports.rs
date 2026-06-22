use std::sync::LazyLock;

use regex::Regex;

use super::W;
use crate::resolution::types::ReExport;
use crate::types::Language;

/// Strip JS line + block comments from `content` while preserving
/// string literals (so `"//"` inside a string stays intact). Used by
/// [`extract_re_exports`] so commented-out export-from statements
/// don't generate phantom re-export edges.
///
/// Scanner is deliberately small: it only tracks the three contexts
/// relevant for JS/TS — single-quote string, double-quote string, and
/// template literal. Comment recognition is the JS spec subset, no
/// regex-literal awareness (which is fine for our use case: we don't
/// apply this to function bodies, only to top-level files).
pub(in crate::resolution::import_resolver) fn strip_js_comments(content: &str) -> String {
    let bytes = content.as_bytes();
    let mut out = String::with_capacity(content.len());
    let mut i = 0;
    let mut str_delim: Option<u8> = None;
    while i < bytes.len() {
        let ch = bytes[i];
        if let Some(delim) = str_delim {
            if ch == b'\\' && i + 1 < bytes.len() {
                out.push('\\');
                let next_len = utf8_len(bytes[i + 1]);
                out.push_str(&content[i + 1..i + 1 + next_len]);
                i += 1 + next_len;
                continue;
            }
            if ch == delim {
                str_delim = None;
            }
            let ch_len = utf8_len(ch);
            out.push_str(&content[i..i + ch_len]);
            i += ch_len;
            continue;
        }
        if ch == b'"' || ch == b'\'' || ch == b'`' {
            str_delim = Some(ch);
            out.push(ch as char);
            i += 1;
            continue;
        }
        if ch == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if ch == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i += 2;
            while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        let ch_len = utf8_len(ch);
        out.push_str(&content[i..i + ch_len]);
        i += ch_len;
    }
    out
}

/// Length in bytes of the UTF-8 sequence starting with `first_byte`.
fn utf8_len(first_byte: u8) -> usize {
    match first_byte {
        b if b < 0x80 => 1,
        b if b >= 0xF0 => 4,
        b if b >= 0xE0 => 3,
        _ => 2,
    }
}

/// Extract JS/TS re-export declarations from `content`.
///
/// Recognised forms:
///   export { foo } from './a';
///   export { foo as bar } from './a';
///   export * from './a';
///   export * as ns from './a';   (treated as wildcard for chasing)
///   export { default as Foo } from './a';
///
/// The walker intentionally stays regex-based — the import-resolver
/// elsewhere in this file already chooses regex over a fresh
/// tree-sitter pass, and this function shares that trade-off. Errors
/// fall through silently; resolution simply skips the broken file.
pub fn extract_re_exports(content: &str, language: Language) -> Vec<ReExport> {
    if !matches!(
        language,
        Language::Typescript | Language::Javascript | Language::Tsx | Language::Jsx
    ) {
        return Vec::new();
    }

    static WILDCARD_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&format!(
            r#"export\s*\*(?:\s+as\s+{W}+)?\s*from\s*['"]([^'"]+)['"]"#
        ))
        .expect("valid regex")
    });
    static NAMED_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"export\s*\{([^}]+)\}\s*from\s*['"]([^'"]+)['"]"#).expect("valid regex")
    });
    static ALIAS_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(&format!(r"^({W}+)\s+as\s+({W}+)$")).expect("valid regex"));
    static IDENT_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(&format!(r"^{W}+$")).expect("valid regex"));

    let mut out: Vec<ReExport> = Vec::new();

    // Pre-strip block comments + line comments so a commented-out
    // `// export { x } from '...'` doesn't produce a phantom edge.
    // (Template literals are still a possible source of false positives;
    // a project that builds export statements as runtime strings is
    // out of scope.)
    let cleaned = strip_js_comments(content);

    // Wildcard: `export * from '...'` or `export * as ns from '...'`
    for m in WILDCARD_RE.captures_iter(&cleaned) {
        out.push(ReExport::Wildcard {
            source: m.get(1).expect("group 1").as_str().to_string(),
        });
    }

    // Named: `export { a, b as c } from '...'`
    for m in NAMED_RE.captures_iter(&cleaned) {
        let inner = m.get(1).expect("group 1").as_str();
        let source = m.get(2).expect("group 2").as_str();
        for raw in inner.split(',') {
            let item = raw.trim();
            if item.is_empty() {
                continue;
            }
            if let Some(alias) = ALIAS_RE.captures(item) {
                out.push(ReExport::Named {
                    exported_name: alias.get(2).expect("group 2").as_str().to_string(),
                    original_name: alias.get(1).expect("group 1").as_str().to_string(),
                    source: source.to_string(),
                });
            } else if IDENT_RE.is_match(item) {
                out.push(ReExport::Named {
                    exported_name: item.to_string(),
                    original_name: item.to_string(),
                    source: source.to_string(),
                });
            }
        }
    }

    out
}

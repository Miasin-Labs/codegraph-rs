use std::sync::LazyLock;

use regex::Regex;

pub(super) fn blank_range(bytes: &mut [u8], start: usize, end: usize) {
    for byte in &mut bytes[start..end] {
        if !matches!(*byte, b'\n' | b'\r') {
            *byte = b' ';
        }
    }
}

pub(super) fn finish(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).expect("blanking ASCII-delimited spans preserves UTF-8")
}

pub(super) fn balanced_paren_end(source: &str, open: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut depth = 0usize;
    let mut index = open;
    while index < bytes.len() {
        match bytes[index] {
            b'"' | b'\'' => {
                let quote = bytes[index];
                index += 1;
                while index < bytes.len() && bytes[index] != quote {
                    if bytes[index] == b'\\' {
                        index += 1;
                    }
                    index += 1;
                }
            }
            b'(' => depth += 1,
            b')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index + 1);
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

static ANNOTATION_HEAD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^([ \t]*)([A-Z][A-Z0-9_]{2,})([ \t]*)\(").expect("valid annotation head regex")
});

pub(super) fn blank_cpp_annotation_macro_calls(source: &str) -> String {
    let mut bytes = source.as_bytes().to_vec();
    for captures in ANNOTATION_HEAD_RE.captures_iter(source) {
        let Some(matched) = captures.get(0) else {
            continue;
        };
        let Some(name) = captures.get(2) else {
            continue;
        };
        let open = matched.end() - 1;
        let Some(end) = balanced_paren_end(source, open) else {
            continue;
        };
        let mut after = end;
        while after < source.len() && source.as_bytes()[after].is_ascii_whitespace() {
            after += 1;
        }
        if source
            .as_bytes()
            .get(after)
            .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(*byte, b'_' | b'~' | b'#'))
        {
            blank_range(&mut bytes, name.start(), end);
        }
    }
    finish(bytes)
}

static LONE_MACRO_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[ \t]*([A-Z][A-Z0-9_]{3,})[ \t]*(?://[^\n\r]*|/\*[^\n\r]*\*/[ \t]*)?\r?$")
        .expect("valid lone macro regex")
});

pub(super) fn blank_lone_macro_lines(source: &str) -> String {
    let mut lines: Vec<&str> = source.split('\n').collect();
    let mut replacements = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let Some(captures) = LONE_MACRO_RE.captures(line) else {
            continue;
        };
        let Some(name) = captures.get(1) else {
            continue;
        };
        if !name.as_str().contains('_') {
            continue;
        }
        let previous = lines[..index]
            .iter()
            .rev()
            .map(|line| line.trim_end_matches('\r').trim())
            .find(|line| !line.is_empty());
        if previous.is_some_and(|line| {
            line.as_bytes().last().is_some_and(|last| {
                matches!(
                    last,
                    b'=' | b'+'
                        | b'-'
                        | b'*'
                        | b'/'
                        | b'%'
                        | b'&'
                        | b'|'
                        | b'^'
                        | b'<'
                        | b'>'
                        | b'?'
                        | b':'
                        | b','
                        | b'('
                        | b'\\'
                )
            })
        }) {
            continue;
        }
        let next = lines[index + 1..]
            .iter()
            .map(|line| line.trim_end_matches('\r').trim())
            .find(|line| !line.is_empty());
        if next.is_some_and(|line| {
            !line.as_bytes().first().is_some_and(|first| {
                first.is_ascii_alphabetic() || matches!(first, b'_' | b'#' | b'{' | b'}' | b'~')
            })
        }) {
            continue;
        }
        replacements.push((index, name.start(), name.end()));
    }
    if replacements.is_empty() {
        return source.to_string();
    }
    let owned: Vec<String> = lines.iter().map(|line| (*line).to_string()).collect();
    let mut owned = owned;
    for (index, start, end) in replacements {
        owned[index].replace_range(start..end, &" ".repeat(end - start));
    }
    lines.clear();
    owned.join("\n")
}

static C_LEADING_ATTR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^([ \t]*)([A-Z][A-Z0-9_]{2,})([ \t]+[A-Za-z_]\w*[ \t*]+[A-Za-z_]\w*[ \t]*\()")
        .expect("valid C leading attribute regex")
});

pub(super) fn blank_c_leading_attr_macros(source: &str) -> String {
    let mut bytes = source.as_bytes().to_vec();
    for captures in C_LEADING_ATTR_RE.captures_iter(source) {
        if let Some(name) = captures.get(2) {
            blank_range(&mut bytes, name.start(), name.end());
        }
    }
    finish(bytes)
}

pub(super) fn restore_directive_lines(original: &str, blanked: &str) -> String {
    if original == blanked || !original.contains('#') {
        return blanked.to_string();
    }
    let original_lines: Vec<&str> = original.split('\n').collect();
    let mut blanked_lines: Vec<&str> = blanked.split('\n').collect();
    let mut continuation = false;
    for (index, line) in original_lines.iter().enumerate().take(blanked_lines.len()) {
        let trimmed = line.trim_start_matches([' ', '\t']);
        let directive = continuation || trimmed.starts_with('#');
        if directive {
            blanked_lines[index] = line;
        }
        continuation = directive && line.trim_end_matches('\r').trim_end().ends_with('\\');
    }
    blanked_lines.join("\n")
}

/// Byte spans of every C++ raw-string literal in `source`, inclusive of the
/// `R"delim(` prefix and the `)delim"` suffix.
///
/// The macro-blanking passes match on lexical shape, so an ALL-CAPS
/// `MACRO(` token or a lone ALL-CAPS line *inside* a raw string
/// (`R"SQL(... CALL_SOMETHING(arg ...)SQL"`) looks exactly like real code to
/// them. `balanced_paren_end` then scans past the raw string's closing
/// delimiter, blanks it, and the resulting parse loses the delimiter — so
/// tree-sitter drops into error recovery and every declaration *after* the
/// raw string vanishes (upstream #1505: functions after a large anonymous
/// namespace of raw strings were never indexed). Recording the spans lets the
/// pipeline restore the original bytes after blanking, exactly as
/// `restore_directive_lines` protects `#`-directives.
///
/// The scan is a single lexer pass that skips line/block comments, character
/// literals, and ordinary string literals, so a literal `R"(` that appears
/// inside a comment or a normal string is never mistaken for a raw-string
/// opener.
fn raw_string_spans(source: &str) -> Vec<(usize, usize)> {
    let bytes = source.as_bytes();
    let len = bytes.len();
    let mut spans = Vec::new();
    let mut index = 0usize;
    let is_ident = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    while index < len {
        match bytes[index] {
            b'/' if index + 1 < len && bytes[index + 1] == b'/' => {
                index += 2;
                while index < len && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if index + 1 < len && bytes[index + 1] == b'*' => {
                index += 2;
                while index + 1 < len && !(bytes[index] == b'*' && bytes[index + 1] == b'/') {
                    index += 1;
                }
                index += 2;
            }
            b'\'' => {
                index += 1;
                while index < len && bytes[index] != b'\'' {
                    if bytes[index] == b'\\' {
                        index += 1;
                    }
                    index += 1;
                }
                index += 1;
            }
            b'R' | b'u' | b'U' | b'L'
                if is_raw_string_opener(bytes, index)
                    && (index == 0 || !is_ident(bytes[index - 1])) =>
            {
                let quote = index + raw_prefix_len(bytes, index);
                // quote points at the opening `"`. Read the delimiter up to `(`.
                let mut cursor = quote + 1;
                let delim_start = cursor;
                while cursor < len && bytes[cursor] != b'(' {
                    cursor += 1;
                }
                if cursor >= len {
                    index += 1;
                    continue;
                }
                let delimiter = &source[delim_start..cursor];
                let closing = format!("){delimiter}\"");
                let body_start = cursor + 1;
                if let Some(rel) = source[body_start..].find(&closing) {
                    let end = body_start + rel + closing.len();
                    spans.push((index, end));
                    index = end;
                } else {
                    // Unterminated raw string: protect the remainder.
                    spans.push((index, len));
                    index = len;
                }
            }
            b'"' => {
                index += 1;
                while index < len && bytes[index] != b'"' {
                    if bytes[index] == b'\\' {
                        index += 1;
                    }
                    index += 1;
                }
                index += 1;
            }
            _ => index += 1,
        }
    }
    spans
}

/// Length of a raw-string prefix (`R`, `LR`, `uR`, `UR`, `u8R`) ending at the
/// opening `"`, given `start` points at the first prefix byte.
fn raw_prefix_len(bytes: &[u8], start: usize) -> usize {
    // The opener has already been validated; find the `R"` and return the
    // count of bytes from `start` up to and including the `"`.
    let mut cursor = start;
    while bytes.get(cursor) != Some(&b'R') {
        cursor += 1;
    }
    cursor + 2 - start
}

/// True if a raw-string literal opens at `start`: an optional encoding prefix
/// (`u8`, `u`, `U`, `L`) followed by `R"`.
fn is_raw_string_opener(bytes: &[u8], start: usize) -> bool {
    let rest = &bytes[start..];
    for prefix in [b"u8R\"".as_slice(), b"uR\"", b"UR\"", b"LR\"", b"R\""] {
        if rest.starts_with(prefix) {
            return true;
        }
    }
    false
}

/// Restore the original bytes of every C++ raw-string span into `blanked`,
/// undoing any blanking that scanned into raw-string content. All blanking
/// passes are byte-offset preserving, so the spans computed from `original`
/// line up exactly in `blanked`.
pub(super) fn restore_raw_string_spans(original: &str, blanked: &str) -> String {
    let spans = raw_string_spans(original);
    if spans.is_empty() {
        return blanked.to_string();
    }
    let original_bytes = original.as_bytes();
    let mut bytes = blanked.as_bytes().to_vec();
    for (start, end) in spans {
        let end = end.min(bytes.len()).min(original_bytes.len());
        if start < end {
            bytes[start..end].copy_from_slice(&original_bytes[start..end]);
        }
    }
    finish(bytes)
}

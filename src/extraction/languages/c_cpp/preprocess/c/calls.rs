use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

use super::super::common::{balanced_paren_end, blank_range, finish};

static C_STATEMENT_HEAD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[ \t]+([a-z_][a-z0-9_]*)[ \t]*\(").expect("valid statement macro regex")
});

pub(in super::super) fn blank_c_statement_macro_calls(source: &str) -> String {
    let keywords: HashSet<&str> = [
        "if", "while", "for", "switch", "return", "do", "else", "sizeof",
    ]
    .into_iter()
    .collect();
    let mut lines: Vec<String> = source.split('\n').map(str::to_string).collect();
    let mut index = 0usize;
    while index < lines.len() {
        let Some(captures) = C_STATEMENT_HEAD_RE.captures(&lines[index]) else {
            index += 1;
            continue;
        };
        let name = captures.get(1).expect("capture exists");
        if keywords.contains(name.as_str()) {
            index += 1;
            continue;
        }
        let name_start = name.start();
        let original_tail = lines[index][name_start..].to_string();
        let mut span = original_tail;
        let mut end_line = index;
        while balanced_paren_end(&span, span.find('(').expect("head contains opening paren"))
            .is_none()
            && end_line < index + 5
            && end_line + 1 < lines.len()
        {
            end_line += 1;
            span.push('\n');
            span.push_str(&lines[end_line]);
        }
        let Some(close) =
            balanced_paren_end(&span, span.find('(').expect("head contains opening paren"))
        else {
            index += 1;
            continue;
        };
        if span[..close].contains([';', '{', '}']) {
            index += 1;
            continue;
        }
        let after = span[close..].trim_end_matches('\r').trim();
        let accepted = if after.is_empty() {
            lines[end_line + 1..]
                .iter()
                .map(|line| line.trim_end_matches('\r').trim())
                .find(|line| !line.is_empty())
                .and_then(|line| line.as_bytes().first())
                .is_some_and(|first| first.is_ascii_alphabetic() || matches!(first, b'_' | b'{'))
        } else {
            after == "{"
        };
        if !accepted {
            index += 1;
            continue;
        }
        let mut consumed = close;
        for (line_index, line) in lines.iter_mut().enumerate().take(end_line + 1).skip(index) {
            let start = if line_index == index { name_start } else { 0 };
            let available = line.len() - start;
            let amount = consumed.min(available);
            let mut bytes = line.as_bytes().to_vec();
            blank_range(&mut bytes, start, start + amount);
            *line = finish(bytes);
            consumed = consumed.saturating_sub(available + 1);
        }
        index = end_line + 1;
    }
    lines.join("\n")
}

static C_TYPE_HEAD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b([A-Za-z_]\w*)[ \t]*\(").expect("valid C call head regex"));
static C_TYPE_OPENER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(struct|union|enum)([ \t\r\n]+)([A-Za-z_]\w*)([ \t\r\n]*\*+)?([ \t\r\n]*[,)])")
        .expect("valid C type argument regex")
});

pub(in super::super) fn blank_c_type_keyword_args(source: &str) -> String {
    let exclusions: HashSet<&str> = [
        "sizeof",
        "alignof",
        "_Alignof",
        "typeof",
        "__typeof__",
        "__typeof",
        "offsetof",
        "_Generic",
        "va_arg",
        "if",
        "while",
        "for",
        "switch",
        "case",
    ]
    .into_iter()
    .collect();
    let mut bytes = source.as_bytes().to_vec();
    for captures in C_TYPE_HEAD_RE.captures_iter(source) {
        let matched = captures.get(0).expect("capture exists");
        let head = captures.get(1).expect("capture exists").as_str();
        if exclusions.contains(head) || declaration_precedes(source, matched.start()) {
            continue;
        }
        let open = matched.end() - 1;
        let mut depth = 1usize;
        let mut at_arg_start = true;
        let mut index = open + 1;
        while index < source.len() && index - open <= 600 {
            match source.as_bytes()[index] {
                b'"' | b'\'' => {
                    let quote = source.as_bytes()[index];
                    index += 1;
                    while index < source.len() && source.as_bytes()[index] != quote {
                        if source.as_bytes()[index] == b'\\' {
                            index += 1;
                        }
                        index += 1;
                    }
                    at_arg_start = false;
                }
                byte if byte.is_ascii_whitespace() => {}
                b'(' => {
                    depth += 1;
                    at_arg_start = false;
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    at_arg_start = false;
                }
                b';' | b'{' | b'}' => break,
                b',' => {
                    if depth == 1 {
                        at_arg_start = true;
                    }
                }
                _ if depth == 1 && at_arg_start => {
                    if let Some(opener) = C_TYPE_OPENER_RE.captures(&source[index..]) {
                        let keyword = opener.get(1).expect("capture exists");
                        blank_range(&mut bytes, index, index + keyword.as_str().len());
                        if let Some(stars) = opener.get(4) {
                            let star_count = stars
                                .as_str()
                                .bytes()
                                .rev()
                                .take_while(|b| *b == b'*')
                                .count();
                            blank_range(
                                &mut bytes,
                                index + stars.end() - star_count,
                                index + stars.end(),
                            );
                        }
                    }
                    at_arg_start = false;
                }
                _ => at_arg_start = false,
            }
            index += 1;
        }
    }
    finish(bytes)
}

fn declaration_precedes(source: &str, start: usize) -> bool {
    let prefix = source[..start].trim_end_matches([' ', '\t']);
    let Some(last) = prefix.as_bytes().last() else {
        return false;
    };
    if !last.is_ascii_alphanumeric() && !matches!(last, b'_' | b'*') {
        return false;
    }
    prefix
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .next_back()
        != Some("return")
}

static C_VA_ARG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bva_arg[ \t]*\(([^(),]+)(,[^()]*)\)").expect("valid va_arg regex")
});
static SINGLE_TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^,[ \t]*[A-Za-z_]\w*[ \t]*$").expect("valid token regex"));

pub(in super::super) fn blank_c_va_arg_qualified_type_args(source: &str) -> String {
    let mut bytes = source.as_bytes().to_vec();
    for captures in C_VA_ARG_RE.captures_iter(source) {
        let rest = captures.get(2).expect("capture exists");
        if !SINGLE_TOKEN_RE.is_match(rest.as_str()) {
            blank_range(&mut bytes, rest.start(), rest.end());
        }
    }
    finish(bytes)
}

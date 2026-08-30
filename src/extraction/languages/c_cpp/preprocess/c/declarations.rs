use std::sync::LazyLock;

use regex::Regex;

use super::super::common::{balanced_paren_end, blank_range, finish};

static PREFIXED_DECL_HEAD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[ \t]*(?:static|extern)[ \t]+[A-Z][A-Z0-9_]{2,}[ \t]*\(")
        .expect("valid prefixed declaration regex")
});

pub(in super::super) fn blank_c_file_scope_prefixed_decl_macros(source: &str) -> String {
    let mut lines: Vec<String> = source.split('\n').map(str::to_string).collect();
    for line in &mut lines {
        let Some(head) = PREFIXED_DECL_HEAD_RE.find(line) else {
            continue;
        };
        let open = head.end() - 1;
        let Some(close) = balanced_paren_end(line, open) else {
            continue;
        };
        if line[close..].trim_end_matches('\r').trim() == ";" {
            let mut bytes = line.as_bytes().to_vec();
            blank_range(&mut bytes, 0, line.len());
            *line = finish(bytes);
        }
    }
    lines.join("\n")
}

static PREFIXED_INIT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^([ \t]*)static[ \t]+[A-Z][A-Z0-9_]{2,}[ \t]*\(([^();'"]*?),[ \t]*([A-Za-z_]\w*)[ \t]*\)([ \t]*=[ \t]*\{?[ \t]*\r?)$"#)
        .expect("valid prefixed initializer regex")
});
static TYPE_RUN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_ \t*]*$").expect("valid type run regex"));

pub(in super::super) fn rewrite_c_prefixed_decl_macro_initializers(source: &str) -> String {
    let mut lines: Vec<String> = source.split('\n').map(str::to_string).collect();
    for line in &mut lines {
        let Some(captures) = PREFIXED_INIT_RE.captures(line) else {
            continue;
        };
        let arg = captures
            .get(2)
            .expect("capture exists")
            .as_str()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if !TYPE_RUN_RE.is_match(&arg) {
            continue;
        }
        let name = captures.get(3).expect("capture exists");
        let tail = captures.get(4).expect("capture exists");
        let prefix = format!(
            "{}static {arg}",
            captures.get(1).expect("capture exists").as_str()
        );
        if prefix.len() + 1 > name.start() {
            continue;
        }
        *line = format!(
            "{}{}{}{}{}",
            prefix,
            " ".repeat(name.start() - prefix.len()),
            name.as_str(),
            " ".repeat(tail.start() - name.end()),
            &line[tail.start()..]
        );
    }
    lines.join("\n")
}

static NAMED_VARIADIC_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([ \t]*#[ \t]*define[ \t]+[A-Za-z_]\w*\([^()\n]*?\b\w+)\.\.\.")
        .expect("valid named variadic regex")
});

pub(in super::super) fn blank_c_named_variadic_define_dots(source: &str) -> String {
    let mut lines: Vec<String> = source.split('\n').map(str::to_string).collect();
    for line in &mut lines {
        let Some(captures) = NAMED_VARIADIC_RE.captures(line) else {
            continue;
        };
        let keep = captures.get(1).expect("capture exists").end();
        line.replace_range(keep..keep + 3, "   ");
    }
    lines.join("\n")
}

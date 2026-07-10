use std::cell::RefCell;
use std::collections::HashMap;

use regex::Regex;

use crate::resolution::types::{ResolutionContext, UnresolvedRef};
use crate::types::{Language, NodeKind};

const MAX_SOURCE_LINE_BYTES: usize = 10_000;
const PATTERN_CACHE_LIMIT: usize = 65_536;

thread_local! {
    static PATTERN_CACHE: RefCell<HashMap<String, Vec<Regex>>> =
        RefCell::new(HashMap::new());
}

fn compile(pattern: String) -> Regex {
    Regex::new(&pattern).expect("valid receiver inference regex")
}

/// Per-language patterns that recover the type of a local variable or typed
/// parameter. Every pattern places the inferred type in capture group 1.
fn build_local_receiver_type_patterns(language: Language, receiver: &str) -> Vec<Regex> {
    let r = regex::escape(receiver);
    let p = |pattern: String| compile(pattern);

    match language.as_str() {
        "typescript" | "javascript" | "tsx" | "jsx" | "arkts" | "svelte" | "vue" => vec![
            p(format!(r"\b{r}\b\s*=\s*new\s+([A-Za-z_$][0-9A-Za-z_.$]*)")),
            p(format!(r"\b{r}\b\s*:\s*([A-Z][0-9A-Za-z_.$]*)")),
        ],
        "python" => vec![
            p(format!(r"\b{r}\b\s*=\s*([A-Z][0-9A-Za-z_.]*)\s*\(")),
            p(format!(r"\b{r}\b\s*:\s*([A-Z][0-9A-Za-z_.]*)")),
        ],
        "java" | "csharp" | "apex" => vec![
            p(format!(r"\b{r}\b\s*=\s*new\s+([A-Za-z_][0-9A-Za-z_.]*)")),
            p(format!(r"\b([A-Z][0-9A-Za-z_.]*)\s+{r}\b\s*[=;,) ]")),
        ],
        "kotlin" | "swift" => vec![
            p(format!(r"\b{r}\b\s*=\s*([A-Z][0-9A-Za-z_.]*)\s*\(")),
            p(format!(r"\b{r}\b\s*:\s*([A-Z][0-9A-Za-z_.]*)")),
        ],
        "rust" => vec![
            p(format!(
                r"\blet\s+(?:mut\s+)?{r}\b(?:\s*:[^=]+)?=\s*&?(?:mut\s+)?([A-Z][0-9A-Za-z_]*)"
            )),
            p(format!(r"\b{r}\s*:\s*&?(?:mut\s+)?([A-Z][0-9A-Za-z_]*)")),
        ],
        "go" => vec![
            p(format!(r"\b{r}\b\s*:=\s*&?([A-Za-z_][0-9A-Za-z_.]*)\s*\{{")),
            p(format!(r"\bvar\s+{r}\s+\*?([A-Za-z_][0-9A-Za-z_.]*)")),
            p(format!(r"\b{r}\s+\*?([A-Z][0-9A-Za-z_.]*)")),
        ],
        "ruby" => vec![p(format!(r"\b{r}\b\s*=\s*([A-Z][0-9A-Za-z_:]*)\.new\b"))],
        "scala" => vec![
            p(format!(r"\b{r}\b\s*=\s*(?:new\s+)?([A-Z][0-9A-Za-z_.]*)")),
            p(format!(r"\b{r}\b\s*:\s*([A-Z][0-9A-Za-z_.]*)")),
        ],
        "dart" => vec![
            p(format!(r"\b{r}\b\s*=\s*([A-Z][0-9A-Za-z_.]*)\s*\(")),
            p(format!(r"\b([A-Z][0-9A-Za-z_.]*)\s+{r}\b\s*[=;,) ]")),
        ],
        "php" => vec![
            p(format!(
                r"\$?{r}\b\s*=\s*new\s+([A-Za-z_\\][0-9A-Za-z_\\]*)"
            )),
            p(format!(r"\b([A-Za-z_\\][0-9A-Za-z_\\]*)\s+&?\${r}\b")),
        ],
        "lua" | "luau" => vec![
            p(format!(r"\b{r}\b\s*=\s*([A-Z][0-9A-Za-z_]*)\.new\b")),
            p(format!(r"\b{r}\b\s*=\s*([A-Z][0-9A-Za-z_]*)\s*\(")),
            // Require an annotation delimiter after the type. This prevents a
            // method call such as `logger:Log()` from self-matching as `Log`.
            p(format!(
                r"\b{r}\b\s*:\s*([A-Z][0-9A-Za-z_.]*)\s*(?:[,)=]|$)"
            )),
        ],
        "r" => vec![p(format!(
            r"\b{r}\b\s*(?:<-|<<-|=)\s*([A-Z][0-9A-Za-z_.]*)\$new\b"
        ))],
        "pascal" => vec![
            p(format!(r"\b{r}\b\s*:\s*([A-Z][0-9A-Za-z_]*)")),
            p(format!(r"\b{r}\b\s*:=\s*([A-Z][0-9A-Za-z_.]*)\.Create\b")),
        ],
        "cfml" | "cfscript" => vec![
            p(format!(
                r"(?i)\b{r}\b\s*=\s*new\s+([A-Za-z_][0-9A-Za-z_.]*)"
            )),
            p(format!(
                r#"(?i)\b{r}\b\s*=\s*createobject\s*\(\s*["']component["']\s*,\s*["']([0-9A-Za-z_.]+)["']"#
            )),
            p(format!(
                r#"(?i)\b{r}\b\s*=\s*createobject\s*\(\s*["']([0-9A-Za-z_.]+)["']\s*\)"#
            )),
            p(format!(r"\b([A-Z][0-9A-Za-z_.]*)\s+{r}\b\s*[=;,) ]")),
            p(format!(
                r#"(?i)\bcfargument[^>\n]*\bname\s*=\s*["']{r}["'][^>\n]*\btype\s*=\s*["']([0-9A-Za-z_.]+)["']"#
            )),
            p(format!(
                r#"(?i)\bcfargument[^>\n]*\btype\s*=\s*["']([0-9A-Za-z_.]+)["'][^>\n]*\bname\s*=\s*["']{r}["']"#
            )),
            p(format!(
                r#"(?i)\b(?:cf)?property\b[^;\n]*\bname\s*=\s*["']{r}["'][^;\n]*\b(?:type|inject)\s*=\s*["']([0-9A-Za-z_.]+)["']"#
            )),
            p(format!(
                r#"(?i)\b(?:cf)?property\b[^;\n]*\b(?:type|inject)\s*=\s*["']([0-9A-Za-z_.]+)["'][^;\n]*\bname\s*=\s*["']{r}["']"#
            )),
        ],
        _ => Vec::new(),
    }
}

fn local_receiver_type_patterns(language: Language, receiver: &str) -> Vec<Regex> {
    let key = format!("{}\0{receiver}", language.as_str());
    PATTERN_CACHE.with(|cache| {
        if let Some(patterns) = cache.borrow().get(&key) {
            return patterns.clone();
        }
        let patterns = build_local_receiver_type_patterns(language, receiver);
        let mut cache = cache.borrow_mut();
        if cache.len() >= PATTERN_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(key, patterns.clone());
        patterns
    })
}

fn normalize_inferred_type_name(raw: &str) -> Option<String> {
    let without_generics = Regex::new(r"<[^>]*>")
        .expect("valid generic regex")
        .replace_all(raw, "");
    let cleaned = without_generics.replace(['&', '*'], "");
    let cleaned = cleaned
        .trim()
        .strip_prefix("mut ")
        .unwrap_or(cleaned.trim());
    let segment = cleaned
        .split(['.', ':'])
        .rfind(|part| !part.trim().is_empty())?
        .trim();
    if segment.is_empty()
        || matches!(
            segment,
            "this"
                | "self"
                | "super"
                | "new"
                | "return"
                | "await"
                | "yield"
                | "typeof"
                | "null"
                | "nil"
                | "None"
                | "true"
                | "false"
                | "True"
                | "False"
                | "undefined"
        )
    {
        return None;
    }
    Some(segment.to_string())
}

fn enclosing_scope_start_line(reference: &UnresolvedRef, context: &dyn ResolutionContext) -> u32 {
    context
        .get_nodes_in_file(&reference.file_path)
        .into_iter()
        .filter(|node| {
            matches!(node.kind, NodeKind::Function | NodeKind::Method)
                && node.language == reference.language
                && node.start_line <= reference.line
                && node.end_line.max(node.start_line) >= reference.line
        })
        .map(|node| node.start_line)
        .max()
        .unwrap_or(1)
}

pub(in crate::resolution::name_matcher) fn infer_local_receiver_type(
    receiver_name: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let mut scan_receiver = receiver_name;
    let mut component_scoped = false;
    if matches!(reference.language.as_str(), "cfml" | "cfscript") {
        if let Some((scope, name)) = receiver_name.split_once('.') {
            if matches!(
                scope.to_ascii_lowercase().as_str(),
                "variables" | "this" | "local" | "arguments"
            ) {
                scan_receiver = name;
                component_scoped =
                    scope.eq_ignore_ascii_case("variables") || scope.eq_ignore_ascii_case("this");
            }
        }
    }

    let patterns = local_receiver_type_patterns(reference.language, scan_receiver);
    if patterns.is_empty() {
        return None;
    }

    let source = context.read_file(&reference.file_path)?;
    let lines: Vec<&str> = source
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    if lines.is_empty() {
        return None;
    }

    let call_index = reference.line.saturating_sub(1) as usize;
    let call_index = call_index.min(lines.len() - 1);
    let start_index = if component_scoped {
        0
    } else {
        enclosing_scope_start_line(reference, context).saturating_sub(1) as usize
    };

    let match_line = |line: &str| -> Option<String> {
        if line.is_empty() || line.len() > MAX_SOURCE_LINE_BYTES {
            return None;
        }
        patterns.iter().find_map(|pattern| {
            pattern
                .captures(line)
                .and_then(|captures| captures.get(1))
                .and_then(|capture| normalize_inferred_type_name(capture.as_str()))
        })
    };

    for index in (start_index.min(call_index)..=call_index).rev() {
        if let Some(inferred) = match_line(lines[index]) {
            return Some(inferred);
        }
    }

    if component_scoped {
        for line in lines.iter().skip(call_index + 1) {
            if let Some(inferred) = match_line(line) {
                return Some(inferred);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::normalize_inferred_type_name;

    #[test]
    fn normalizes_qualified_generic_type_names() {
        assert_eq!(
            normalize_inferred_type_name("pkg.Repository<User>"),
            Some("Repository".into())
        );
        assert_eq!(
            normalize_inferred_type_name("&mut Logger"),
            Some("Logger".into())
        );
        assert_eq!(normalize_inferred_type_name("return"), None);
    }
}

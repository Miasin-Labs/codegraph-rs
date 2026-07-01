use std::collections::HashSet;
use std::path::Path;

use super::super::format::{is_low_value, locale_cmp};
use crate::codegraph::CodeGraph;
use crate::error::Result;
use crate::extraction::is_generated_file;
use crate::search::extract_search_terms_opts;
use crate::utils::validate_existing_path_within_root_real;

const MAX_MATCHING_FILES: usize = 8;
const MAX_LINES_PER_FILE: usize = 3;
const MAX_LINE_CHARS: usize = 240;
const MAX_FILE_BYTES: u64 = 1_000_000;
const MAX_SCANNED_FILES: usize = 2_000;
const MAX_SCANNED_BYTES: u64 = 50_000_000;

#[derive(Clone)]
pub(in crate::mcp::tools::explore) struct LiteralLineMatch {
    pub line_number: usize,
    pub text: String,
    pub terms: Vec<String>,
}

#[derive(Clone)]
pub(in crate::mcp::tools::explore) struct LiteralFileMatch {
    pub file_path: String,
    pub language: String,
    pub lines: Vec<LiteralLineMatch>,
    unique_term_count: usize,
    low_value: bool,
    generated: bool,
}

pub(in crate::mcp::tools::explore) fn collect_literal_content_matches(
    cg: &CodeGraph,
    project_root: &Path,
    query: &str,
) -> Result<Vec<LiteralFileMatch>> {
    let terms = literal_terms(query);
    if terms.is_empty() {
        return Ok(Vec::new());
    }

    let mut files = Vec::new();
    let mut scanned_files = 0usize;
    let mut scanned_bytes = 0u64;
    for file in cg.get_files()? {
        if file.size > MAX_FILE_BYTES {
            continue;
        }
        if scanned_files >= MAX_SCANNED_FILES
            || scanned_bytes.saturating_add(file.size) > MAX_SCANNED_BYTES
        {
            break;
        }
        let Some(abs_path) = validate_existing_path_within_root_real(project_root, &file.path)
        else {
            continue;
        };
        let Ok(content) = std::fs::read_to_string(&abs_path) else {
            continue;
        };
        scanned_files += 1;
        scanned_bytes = scanned_bytes.saturating_add(file.size);

        let mut lines = Vec::new();
        let mut unique_terms = HashSet::new();
        for (index, line) in content.lines().enumerate() {
            let lower = line.to_lowercase();
            let matched_terms: Vec<String> = terms
                .iter()
                .filter(|term| lower.contains(term.as_str()))
                .cloned()
                .collect();
            if matched_terms.is_empty() {
                continue;
            }
            for term in &matched_terms {
                unique_terms.insert(term.clone());
            }
            lines.push(LiteralLineMatch {
                line_number: index + 1,
                text: trim_line(line),
                terms: matched_terms,
            });
            if lines.len() >= MAX_LINES_PER_FILE {
                break;
            }
        }
        if lines.is_empty() {
            continue;
        }
        files.push(LiteralFileMatch {
            low_value: is_low_value(&file.path),
            generated: is_generated_file(&file.path),
            file_path: file.path,
            language: file.language.as_str().to_string(),
            lines,
            unique_term_count: unique_terms.len(),
        });
    }

    files.sort_by(|a, b| {
        a.low_value
            .cmp(&b.low_value)
            .then_with(|| a.generated.cmp(&b.generated))
            .then_with(|| b.unique_term_count.cmp(&a.unique_term_count))
            .then_with(|| b.lines.len().cmp(&a.lines.len()))
            .then_with(|| locale_cmp(&a.file_path, &b.file_path))
    });
    files.truncate(MAX_MATCHING_FILES);
    Ok(files)
}

pub(in crate::mcp::tools::explore) fn append_literal_content_section(
    matches: &[LiteralFileMatch],
    lines: &mut Vec<String>,
) {
    if matches.is_empty() {
        return;
    }
    lines.push("### Literal content matches".to_string());
    lines.push(String::new());
    lines.push("Raw line matches from indexed files. These cover comments, string literals, log messages, TODO/FIXME markers, and other text that is not stored in the symbol FTS index.".to_string());
    lines.push(String::new());
    for file in matches {
        lines.push(format!("**{} ({})**", file.file_path, file.language));
        for hit in &file.lines {
            lines.push(format!(
                "- {}:{} — `{}`",
                file.file_path,
                hit.line_number,
                hit.terms.join("`, `")
            ));
            lines.push(format!("    {}", hit.text.trim()));
        }
        lines.push(String::new());
    }
}

fn literal_terms(query: &str) -> Vec<String> {
    let lower = query.to_lowercase();
    let mut terms = Vec::new();
    let mut seen = HashSet::new();

    for phrase in quoted_phrases(&lower) {
        push_term(&mut terms, &mut seen, phrase);
    }

    for signal in [
        "todo",
        "fixme",
        "hack",
        "xxx",
        "stub",
        "placeholder",
        "not implemented",
        "not-implemented",
        "unimplemented",
        "disabled",
        "temporary",
        "workaround",
        "missing",
        "error",
        "log",
        "panic",
        "throw new error",
        "logger.warn",
        "logger.error",
        "console.warn",
        "console.error",
        "gap",
    ] {
        if lower.contains(signal) {
            push_term(&mut terms, &mut seen, signal.to_string());
        }
    }

    if looks_like_short_literal_query(query) {
        for term in extract_search_terms_opts(query, false) {
            push_term(&mut terms, &mut seen, term);
        }
    }

    terms
}

fn looks_like_short_literal_query(query: &str) -> bool {
    let words = query.split_whitespace().count();
    words > 0 && words <= 6
}

fn quoted_phrases(query: &str) -> Vec<String> {
    let mut phrases = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for ch in query.chars() {
        match quote {
            Some(q) if ch == q => {
                let phrase = current.trim();
                if phrase.chars().count() >= 3 {
                    phrases.push(phrase.to_string());
                }
                current.clear();
                quote = None;
            }
            Some(_) => current.push(ch),
            None if ch == '"' || ch == '\'' || ch == '`' => quote = Some(ch),
            None => {}
        }
    }
    phrases
}

fn push_term(terms: &mut Vec<String>, seen: &mut HashSet<String>, term: String) {
    if term.chars().count() < 3 || !seen.insert(term.clone()) {
        return;
    }
    terms.push(term);
}

fn trim_line(line: &str) -> String {
    if line.chars().count() <= MAX_LINE_CHARS {
        return line.to_string();
    }
    let mut out: String = line.chars().take(MAX_LINE_CHARS).collect();
    out.push_str(" ...");
    out
}

#[cfg(test)]
mod tests {
    use super::literal_terms;

    #[test]
    fn literal_terms_extract_signals_and_quotes() {
        let terms = literal_terms("TODO FIXME `exact log message` throw new Error");
        assert!(terms.contains(&"todo".to_string()));
        assert!(terms.contains(&"fixme".to_string()));
        assert!(terms.contains(&"exact log message".to_string()));
        assert!(terms.contains(&"throw new error".to_string()));
    }

    #[test]
    fn literal_terms_extract_short_raw_queries() {
        let terms = literal_terms("persist project cache gaps");
        assert!(terms.contains(&"persist".to_string()));
        assert!(terms.contains(&"project".to_string()));
        assert!(terms.contains(&"cache".to_string()));
        assert!(terms.contains(&"gaps".to_string()) || terms.contains(&"gap".to_string()));
    }
}

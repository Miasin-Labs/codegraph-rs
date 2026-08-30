use std::collections::HashSet;
use std::path::Path;

use super::super::format::{is_low_value, locale_cmp};
use super::execution::LiteralScanBudget;
use crate::codegraph::CodeGraph;
use crate::error::Result;
use crate::extraction::is_value_sensitive_language;
use crate::search::extract_search_terms_opts;
use crate::utils::resolve_existing_path_within_root_real;

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
    pub match_count: usize,
    pub truncated: bool,
    unique_term_count: usize,
    low_value: bool,
    generated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::mcp::tools::explore) enum LiteralScanLimit {
    Files,
    Bytes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::mcp::tools::explore) enum LiteralScanOutcome {
    Complete {
        scanned_files: usize,
        scanned_bytes: u64,
    },
    Truncated {
        scanned_files: usize,
        scanned_bytes: u64,
        limit: LiteralScanLimit,
    },
}

impl Default for LiteralScanOutcome {
    fn default() -> Self {
        Self::Complete {
            scanned_files: 0,
            scanned_bytes: 0,
        }
    }
}

#[derive(Default)]
pub(in crate::mcp::tools::explore) struct LiteralContentMatches {
    pub files: Vec<LiteralFileMatch>,
    pub total_files: usize,
    pub total_matches: usize,
    pub files_omitted: usize,
    pub scan_outcome: LiteralScanOutcome,
}

impl LiteralContentMatches {
    pub fn is_empty(&self) -> bool {
        self.total_matches == 0
    }

    pub fn scan_was_truncated(&self) -> bool {
        matches!(self.scan_outcome, LiteralScanOutcome::Truncated { .. })
    }

    pub fn scan_truncation_message(&self) -> Option<String> {
        let LiteralScanOutcome::Truncated {
            scanned_files,
            scanned_bytes,
            limit,
        } = self.scan_outcome
        else {
            return None;
        };
        let limit = match limit {
            LiteralScanLimit::Files => "file-count",
            LiteralScanLimit::Bytes => "byte",
        };
        Some(format!(
            "Literal scan stopped at the {limit} limit after {scanned_files} file(s) and {scanned_bytes} byte(s); unscanned files may contain additional matches."
        ))
    }
}

pub(in crate::mcp::tools::explore) fn collect_literal_content_matches(
    cg: &CodeGraph,
    project_root: &Path,
    query: &str,
    budget: LiteralScanBudget,
) -> Result<LiteralContentMatches> {
    let terms = literal_terms(query);
    if terms.is_empty() {
        return Ok(LiteralContentMatches::default());
    }

    let mut files = Vec::new();
    let mut total_files = 0usize;
    let mut total_matches = 0usize;
    let mut scanned_files = 0usize;
    let mut scanned_bytes = 0u64;
    let file_limit = MAX_SCANNED_FILES.min(budget.max_files);
    let byte_limit = MAX_SCANNED_BYTES.min(budget.max_bytes);
    let mut scan_outcome = None;

    let indexed_files = cg.get_files()?;
    let indexed_paths = indexed_files
        .iter()
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    let generated = cg.generated_file_predicate(&indexed_paths)?;
    for file in indexed_files {
        crate::graph::cancel::check()?;
        if is_value_sensitive_language(file.language) {
            continue;
        }
        if scanned_files >= file_limit {
            scan_outcome = Some(LiteralScanOutcome::Truncated {
                scanned_files,
                scanned_bytes,
                limit: LiteralScanLimit::Files,
            });
            break;
        }
        let Some(abs_path) = resolve_existing_path_within_root_real(project_root, &file.path)
        else {
            continue;
        };
        let Ok(metadata) = std::fs::metadata(&abs_path) else {
            continue;
        };
        let actual_size = metadata.len();
        if actual_size > MAX_FILE_BYTES {
            continue;
        }
        if scanned_bytes.saturating_add(actual_size) > byte_limit {
            scan_outcome = Some(LiteralScanOutcome::Truncated {
                scanned_files,
                scanned_bytes,
                limit: LiteralScanLimit::Bytes,
            });
            break;
        }
        let Ok(content) = std::fs::read_to_string(&abs_path) else {
            continue;
        };
        scanned_files += 1;
        scanned_bytes = scanned_bytes.saturating_add(actual_size);

        let mut lines = Vec::new();
        let mut file_match_count = 0usize;
        let mut unique_terms = HashSet::new();
        for (index, line) in content.lines().enumerate() {
            if index % 128 == 0 {
                crate::graph::cancel::check()?;
            }
            let lower = line.to_lowercase();
            let matched_terms: Vec<String> = terms
                .iter()
                .filter(|term| lower.contains(term.as_str()))
                .cloned()
                .collect();
            if matched_terms.is_empty() {
                continue;
            }
            file_match_count += 1;
            for term in &matched_terms {
                unique_terms.insert(term.clone());
            }
            if lines.len() < MAX_LINES_PER_FILE {
                lines.push(LiteralLineMatch {
                    line_number: index + 1,
                    text: trim_line(line),
                    terms: matched_terms,
                });
            }
        }
        if lines.is_empty() {
            continue;
        }
        total_files += 1;
        total_matches += file_match_count;
        files.push(LiteralFileMatch {
            low_value: is_low_value(&file.path),
            generated: generated.is_generated(&file.path),
            file_path: file.path,
            language: file.language.as_str().to_string(),
            lines,
            match_count: file_match_count,
            truncated: file_match_count > MAX_LINES_PER_FILE,
            unique_term_count: unique_terms.len(),
        });
    }

    let scan_outcome = scan_outcome.unwrap_or(LiteralScanOutcome::Complete {
        scanned_files,
        scanned_bytes,
    });

    files.sort_by(|a, b| {
        a.low_value
            .cmp(&b.low_value)
            .then_with(|| a.generated.cmp(&b.generated))
            .then_with(|| b.unique_term_count.cmp(&a.unique_term_count))
            .then_with(|| b.lines.len().cmp(&a.lines.len()))
            .then_with(|| locale_cmp(&a.file_path, &b.file_path))
    });
    let files_omitted = files.len().saturating_sub(MAX_MATCHING_FILES);
    files.truncate(MAX_MATCHING_FILES);
    Ok(LiteralContentMatches {
        files,
        total_files,
        total_matches,
        files_omitted,
        scan_outcome,
    })
}

pub(in crate::mcp::tools::explore) fn append_literal_content_section(
    matches: &LiteralContentMatches,
    lines: &mut Vec<String>,
) {
    if matches.is_empty() {
        if let Some(message) = matches.scan_truncation_message() {
            lines.push(format!("> {message}"));
            lines.push(String::new());
        }
        return;
    }
    lines.push("### Literal content matches".to_string());
    lines.push(String::new());
    lines.push(format!(
        "{} raw line match(es) across {} indexed file(s).",
        matches.total_matches, matches.total_files
    ));
    lines.push(String::new());
    for file in &matches.files {
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
        if file.truncated {
            lines.push(format!(
                "    ... {} total matches in file",
                file.match_count
            ));
        }
        lines.push(String::new());
    }
    if matches.files_omitted > 0 {
        lines.push(format!(
            "... {} matching file(s) omitted; refine the query to inspect them.",
            matches.files_omitted
        ));
        lines.push(String::new());
    }
    if let Some(message) = matches.scan_truncation_message() {
        lines.push(format!("> {message}"));
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

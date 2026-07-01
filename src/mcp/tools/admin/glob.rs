//! Glob pattern conversion for codegraph_files.

use regex::Regex;

use crate::error::{CodeGraphError, Result};

pub(in crate::mcp::tools) fn glob_to_regex(pattern: &str) -> Result<Regex> {
    Regex::new(&glob_to_regex_str(pattern)).map_err(|e| CodeGraphError::other(e.to_string()))
}

fn glob_to_regex_str(pattern: &str) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' if chars.get(i + 1) == Some(&'*') => {
                i += 2;
                if chars.get(i) == Some(&'/') {
                    out.push_str("(?:.*/)?");
                    i += 1;
                } else {
                    out.push_str(".*");
                }
            }
            '*' => {
                out.push_str("[^/]*");
                i += 1;
            }
            '?' => {
                out.push_str("[^/]");
                i += 1;
            }
            '{' => {
                if let Some(close) = chars[i + 1..].iter().position(|c| *c == '}') {
                    let end = i + 1 + close;
                    let body: String = chars[i + 1..end].iter().collect();
                    let alts: Vec<String> = body
                        .split(',')
                        .filter(|alt| !alt.is_empty())
                        .map(regex::escape)
                        .collect();
                    if alts.is_empty() {
                        out.push_str("\\{");
                    } else {
                        out.push_str("(?:");
                        out.push_str(&alts.join("|"));
                        out.push(')');
                        i = end + 1;
                        continue;
                    }
                } else {
                    out.push_str("\\{");
                }
                i += 1;
            }
            c => {
                if regex_special(c) {
                    out.push('\\');
                }
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn regex_special(c: char) -> bool {
    matches!(
        c,
        '.' | '+' | '^' | '$' | '(' | ')' | '|' | '[' | ']' | '\\' | '}'
    )
}

#[cfg(test)]
mod tests {
    use super::glob_to_regex;

    #[test]
    fn supports_brace_extension_groups() {
        let re = glob_to_regex("**/*.{ts,tsx,rs}").unwrap();
        assert!(re.is_match("packages/plugin/src/index.ts"));
        assert!(re.is_match("packages/plugin/src/view.tsx"));
        assert!(re.is_match("packages/dashboard/src/lib.rs"));
        assert!(!re.is_match("packages/plugin/README.md"));
    }

    #[test]
    fn globstar_slash_can_match_root_files() {
        let re = glob_to_regex("**/*.ts").unwrap();
        assert!(re.is_match("src/index.ts"));
        assert!(re.is_match("index.ts"));
    }
}

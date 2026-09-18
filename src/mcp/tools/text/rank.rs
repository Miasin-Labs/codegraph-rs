//! Relevance of text hits: which files lead, and which lines are comments.
//!
//! Per-language behaviour is a static table (comment prefixes by language),
//! like the other rules tables in the codebase — add a language there, not
//! with a branch in the scanner.

use std::cmp::Reverse;

use super::super::format::{is_low_value, is_test_path};
use crate::extraction::is_value_sensitive_language;
use crate::types::Language;

/// What kind of file a hit is in, most relevant first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::mcp::tools) enum FileClass {
    /// Project source.
    Code,
    /// Tests, specs, fixtures, mocks.
    Test,
    /// Docs and configuration (markdown, YAML, TOML, …).
    Docs,
    /// Generated or vendored output.
    Generated,
}

impl FileClass {
    pub fn of(path: &str, language: Language, generated: bool) -> Self {
        if generated {
            Self::Generated
        } else if is_test_path(path) || is_low_value(path) {
            Self::Test
        } else if is_docs_language(language) {
            Self::Docs
        } else {
            Self::Code
        }
    }
}

fn is_docs_language(language: Language) -> bool {
    is_value_sensitive_language(language)
        || matches!(
            language,
            Language::Markdown | Language::Gitignore | Language::Unknown
        )
}

/// Line-comment (and block-comment continuation) prefixes by language.
fn comment_prefixes(language: Language) -> &'static [&'static str] {
    const C_LIKE: &[&str] = &["//", "/*", "*/", "* ", "*\t"];
    const HASH: &[&str] = &["#"];
    const MARKUP: &[&str] = &["<!--"];
    match language {
        Language::Typescript
        | Language::Javascript
        | Language::Tsx
        | Language::Jsx
        | Language::Arkts
        | Language::Go
        | Language::Rust
        | Language::Java
        | Language::C
        | Language::Cpp
        | Language::Csharp
        | Language::Swift
        | Language::Kotlin
        | Language::Dart
        | Language::Scala
        | Language::Objc
        | Language::Solidity
        | Language::Move
        | Language::Cairo
        | Language::Sway
        | Language::Fe
        | Language::Apex
        | Language::Cfscript => C_LIKE,
        Language::Php => &["//", "/*", "*/", "* ", "*\t", "#"],
        Language::Python
        | Language::Ruby
        | Language::R
        | Language::Nix
        | Language::Bash
        | Language::Zsh
        | Language::Fish
        | Language::Yaml
        | Language::Toml
        | Language::Gitignore
        | Language::Vyper => HASH,
        Language::Properties => &["#", "!"],
        Language::Terraform => &["#", "//", "/*", "* "],
        Language::Lua | Language::Luau => &["--"],
        Language::Erlang => &["%"],
        Language::Pascal => &["//", "{", "(*"],
        Language::Vbnet => &["'"],
        Language::Cobol => &["*>"],
        Language::Html
        | Language::Xml
        | Language::Visualforce
        | Language::Aura
        | Language::Markdown
        | Language::Svelte
        | Language::Vue
        | Language::Astro
        | Language::Liquid
        | Language::Twig
        | Language::Razor => MARKUP,
        Language::Cfml | Language::Cfquery => &["<!---", "//"],
        Language::Unknown => &[],
    }
}

/// Whether `line` reads as a comment in `language` (a heuristic on its first
/// non-blank characters; a trailing comment after code counts as code).
pub(in crate::mcp::tools) fn is_comment_line(language: Language, line: &[u8]) -> bool {
    let trimmed = line.trim_ascii_start();
    if trimmed == b"*" {
        return comment_prefixes(language).contains(&"* ");
    }
    comment_prefixes(language)
        .iter()
        .any(|prefix| trimmed.starts_with(prefix.as_bytes()))
}

/// Sort key of a file with hits: code before tests before docs before
/// generated, files with a hit outside comments first, then the most hits,
/// then path order.
pub(in crate::mcp::tools) fn file_rank_key(
    class: FileClass,
    code_hits: usize,
    count: usize,
    path: &str,
) -> (FileClass, bool, Reverse<usize>, &str) {
    (class, code_hits == 0, Reverse(count), path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_order_code_tests_docs_generated() {
        assert_eq!(
            FileClass::of("src/lib.rs", Language::Rust, false),
            FileClass::Code
        );
        assert_eq!(
            FileClass::of("tests/lib_test.rs", Language::Rust, false),
            FileClass::Test
        );
        assert_eq!(
            FileClass::of("src/app.test.ts", Language::Typescript, false),
            FileClass::Test
        );
        assert_eq!(
            FileClass::of("README.md", Language::Markdown, false),
            FileClass::Docs
        );
        assert_eq!(
            FileClass::of("Cargo.toml", Language::Toml, false),
            FileClass::Docs
        );
        assert_eq!(
            FileClass::of("src/gen.rs", Language::Rust, true),
            FileClass::Generated
        );
        assert!(FileClass::Code < FileClass::Test);
        assert!(FileClass::Test < FileClass::Docs);
        assert!(FileClass::Docs < FileClass::Generated);
    }

    #[test]
    fn comment_lines_follow_the_language_table() {
        assert!(is_comment_line(Language::Rust, b"    /// doc"));
        assert!(is_comment_line(Language::Rust, b"// note"));
        assert!(is_comment_line(Language::C, b" * continued block"));
        assert!(is_comment_line(Language::C, b" *"));
        assert!(!is_comment_line(Language::C, b"*ptr = 0;"));
        assert!(!is_comment_line(Language::C, b"#define FOO 1"));
        assert!(is_comment_line(Language::Python, b"  # note"));
        assert!(is_comment_line(Language::Lua, b"-- note"));
        assert!(!is_comment_line(Language::Rust, b"let x = 1; // trailing"));
        assert!(!is_comment_line(Language::Unknown, b"# anything"));
    }

    #[test]
    fn rank_key_prefers_code_hits_then_counts() {
        let mut rows = [
            (FileClass::Test, 5, 5, "tests/a.rs"),
            (FileClass::Code, 0, 9, "src/comments_only.rs"),
            (FileClass::Code, 1, 1, "src/one.rs"),
            (FileClass::Code, 3, 3, "src/three.rs"),
            (FileClass::Docs, 2, 2, "README.md"),
        ];
        rows.sort_by(|a, b| {
            file_rank_key(a.0, a.1, a.2, a.3).cmp(&file_rank_key(b.0, b.1, b.2, b.3))
        });
        let order: Vec<&str> = rows.iter().map(|r| r.3).collect();
        assert_eq!(
            order,
            vec![
                "src/three.rs",
                "src/one.rs",
                "src/comments_only.rs",
                "tests/a.rs",
                "README.md"
            ]
        );
    }
}

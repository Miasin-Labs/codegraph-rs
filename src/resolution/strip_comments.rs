//! Per-language comment stripper for framework route extractors.
//! Port of `src/resolution/strip-comments.ts`.
//!
//! Replaces comment characters and string-literal contents that hide
//! routing-shaped text with spaces (NOT removal) so that source offsets
//! are preserved. This means a regex match offset from the stripped
//! output still maps to the same line in the original source.
//!
//! Example:
//!   Input:  `x = 1  # path('/fake/', V)\n real = 2`
//!   Output: `x = 1                       \n real = 2`
//!
//! Why strip strings/docstrings as well as comments? Python module/class
//! docstrings are a common source of false positives — they often contain
//! `path('/example/', View)` examples in usage docs. We treat triple-quoted
//! strings the same as comments. Single-line strings stay intact (a `#`
//! inside a Python string is NOT a comment).
//!
//! Scope: this is a pragmatic, regex-supporting helper, not a full parser.
//! It does NOT try to detect JS regex literals, Python f-string expressions,
//! or shell-style heredocs. Those edge cases are not load-bearing for the
//! `path(...)`, `Route::get(...)`, `app.get(...)` style patterns that
//! framework extractors scan for.
//!
//! Rust port note: the scanner works on BYTES, not chars. Every delimiter
//! it cares about (quotes, `/`, `*`, `#`, `=`, `\n`, `\\`) is ASCII, and
//! UTF-8 guarantees multi-byte sequences never contain ASCII bytes, so the
//! byte walk is exactly equivalent to the TS UTF-16 walk for this grammar.
//! Blanking replaces each non-newline BYTE with a space, so the output has
//! the identical BYTE length as the input — `regex` match offsets (bytes)
//! stay valid against the original source. Blanked ranges always start and
//! end on ASCII delimiters, so the output is always valid UTF-8.

/// Languages with distinct comment/string grammars (TS `CommentLang` union).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommentLang {
    Python,
    Javascript,
    Typescript,
    Php,
    Ruby,
    Java,
    Csharp,
    Swift,
    Go,
    Rust,
}

pub fn strip_comments_for_regex(content: &str, lang: CommentLang) -> String {
    match lang {
        CommentLang::Python => strip_python(content),
        CommentLang::Ruby => strip_ruby(content),
        CommentLang::Rust => strip_rust(content),
        CommentLang::Php => strip_php(content),
        CommentLang::Go => strip_go(content),
        CommentLang::Javascript | CommentLang::Typescript => {
            strip_c_style(content, /* allow_single_quote_strings */ true)
        }
        CommentLang::Java | CommentLang::Csharp | CommentLang::Swift => {
            strip_c_style(content, /* allow_single_quote_strings */ false)
        }
    }
}

/// Replace every byte in a slice with spaces, but keep newlines so line
/// numbers computed downstream remain valid.
fn blank_range(out: &mut [u8], src: &[u8], start: usize, end: usize) {
    for i in start..end {
        out[i] = if src[i] == b'\n' { b'\n' } else { b' ' };
    }
}

/// `src.as_bytes()[i + offset]` with the TS `src[i + k] ?? ''` semantics:
/// out-of-bounds compares unequal to any delimiter byte (0 is not a
/// delimiter we test for).
#[inline]
fn at(src: &[u8], i: usize) -> u8 {
    src.get(i).copied().unwrap_or(0)
}

fn finish(out: Vec<u8>) -> String {
    // Blanked ranges are delimited by ASCII bytes and whole multi-byte
    // sequences are always blanked together, so this cannot fail.
    String::from_utf8(out).expect("strip-comments output must remain valid UTF-8")
}

// ---------- Python ----------

fn strip_python(source: &str) -> String {
    let src = source.as_bytes();
    let mut out = src.to_vec();
    let n = src.len();
    let mut i = 0usize;

    while i < n {
        let c = src[i];
        let c2 = at(src, i + 1);
        let c3 = at(src, i + 2);

        // Triple-quoted string: """...""" or '''...'''
        if (c == b'"' || c == b'\'') && c2 == c && c3 == c {
            let quote = c;
            let start = i;
            i += 3;
            while i < n {
                if src[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if src[i] == quote && at(src, i + 1) == quote && at(src, i + 2) == quote {
                    i += 3;
                    break;
                }
                i += 1;
            }
            blank_range(&mut out, src, start, i.min(n));
            continue;
        }

        // Single-line string: '...' or "..."
        if c == b'"' || c == b'\'' {
            let quote = c;
            i += 1;
            while i < n && src[i] != quote {
                if src[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if src[i] == b'\n' {
                    break; // unterminated
                }
                i += 1;
            }
            if i < n && src[i] == quote {
                i += 1;
            }
            continue;
        }

        // Line comment
        if c == b'#' {
            let start = i;
            while i < n && src[i] != b'\n' {
                i += 1;
            }
            blank_range(&mut out, src, start, i);
            continue;
        }

        i += 1;
    }

    finish(out)
}

// ---------- Ruby ----------

fn strip_ruby(source: &str) -> String {
    let src = source.as_bytes();
    let mut out = src.to_vec();
    let n = src.len();
    let mut i = 0usize;
    let mut at_line_start = true;

    while i < n {
        let c = src[i];

        // =begin / =end block comments must be at start of line (after optional whitespace)
        if at_line_start && c == b'=' && src[i..].starts_with(b"=begin") {
            let start = i;
            // consume to matching =end at line start
            i += "=begin".len();
            while i < n {
                if src[i] == b'\n' {
                    // check next line for =end
                    let mut j = i + 1;
                    while j < n && (src[j] == b' ' || src[j] == b'\t') {
                        j += 1;
                    }
                    if src[j..].starts_with(b"=end") {
                        i = j + "=end".len();
                        // consume rest of that line
                        while i < n && src[i] != b'\n' {
                            i += 1;
                        }
                        break;
                    }
                }
                i += 1;
            }
            blank_range(&mut out, src, start, i.min(n));
            at_line_start = i > 0 && at(src, i - 1) == b'\n';
            continue;
        }

        // String literals
        if c == b'"' || c == b'\'' {
            let quote = c;
            i += 1;
            while i < n && src[i] != quote {
                if src[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if src[i] == b'\n' {
                    break;
                }
                i += 1;
            }
            if i < n && src[i] == quote {
                i += 1;
            }
            at_line_start = false;
            continue;
        }

        // Line comment
        if c == b'#' {
            let start = i;
            while i < n && src[i] != b'\n' {
                i += 1;
            }
            blank_range(&mut out, src, start, i);
            at_line_start = false;
            continue;
        }

        if c == b'\n' {
            at_line_start = true;
            i += 1;
            continue;
        }
        if c == b' ' || c == b'\t' {
            // whitespace doesn't change at_line_start
            i += 1;
            continue;
        }
        at_line_start = false;
        i += 1;
    }

    finish(out)
}

// ---------- C-style (JS/TS/Java/C#/Swift) ----------

fn strip_c_style(source: &str, allow_single_quote_strings: bool) -> String {
    let src = source.as_bytes();
    let mut out = src.to_vec();
    let n = src.len();
    let mut i = 0usize;

    while i < n {
        let c = src[i];
        let c2 = at(src, i + 1);

        // Block comment
        if c == b'/' && c2 == b'*' {
            let start = i;
            i += 2;
            while i < n && !(src[i] == b'*' && at(src, i + 1) == b'/') {
                i += 1;
            }
            if i < n {
                i += 2;
            }
            blank_range(&mut out, src, start, i.min(n));
            continue;
        }

        // Line comment
        if c == b'/' && c2 == b'/' {
            let start = i;
            while i < n && src[i] != b'\n' {
                i += 1;
            }
            blank_range(&mut out, src, start, i);
            continue;
        }

        // String literals
        if c == b'"' || (allow_single_quote_strings && c == b'\'') || c == b'`' {
            let quote = c;
            i += 1;
            while i < n && src[i] != quote {
                if src[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                // Template literal can span lines; regular strings break on newline (treat as unterminated)
                if quote != b'`' && src[i] == b'\n' {
                    break;
                }
                i += 1;
            }
            if i < n && src[i] == quote {
                i += 1;
            }
            continue;
        }

        i += 1;
    }

    finish(out)
}

// ---------- PHP ----------

fn strip_php(source: &str) -> String {
    let src = source.as_bytes();
    let mut out = src.to_vec();
    let n = src.len();
    let mut i = 0usize;

    while i < n {
        let c = src[i];
        let c2 = at(src, i + 1);

        // Block comment
        if c == b'/' && c2 == b'*' {
            let start = i;
            i += 2;
            while i < n && !(src[i] == b'*' && at(src, i + 1) == b'/') {
                i += 1;
            }
            if i < n {
                i += 2;
            }
            blank_range(&mut out, src, start, i.min(n));
            continue;
        }

        // // line comment
        if c == b'/' && c2 == b'/' {
            let start = i;
            while i < n && src[i] != b'\n' {
                i += 1;
            }
            blank_range(&mut out, src, start, i);
            continue;
        }

        // # line comment (PHP supports both)
        if c == b'#' {
            let start = i;
            while i < n && src[i] != b'\n' {
                i += 1;
            }
            blank_range(&mut out, src, start, i);
            continue;
        }

        // String literals: ', ", ` (PHP doesn't really use backticks for strings,
        // but it does have shell-exec backticks; treating as a string is fine here)
        if c == b'"' || c == b'\'' || c == b'`' {
            let quote = c;
            i += 1;
            while i < n && src[i] != quote {
                if src[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if src[i] == b'\n' {
                    break;
                }
                i += 1;
            }
            if i < n && src[i] == quote {
                i += 1;
            }
            continue;
        }

        i += 1;
    }

    finish(out)
}

// ---------- Go ----------

fn strip_go(source: &str) -> String {
    let src = source.as_bytes();
    let mut out = src.to_vec();
    let n = src.len();
    let mut i = 0usize;

    while i < n {
        let c = src[i];
        let c2 = at(src, i + 1);

        // Block comment
        if c == b'/' && c2 == b'*' {
            let start = i;
            i += 2;
            while i < n && !(src[i] == b'*' && at(src, i + 1) == b'/') {
                i += 1;
            }
            if i < n {
                i += 2;
            }
            blank_range(&mut out, src, start, i.min(n));
            continue;
        }

        // Line comment
        if c == b'/' && c2 == b'/' {
            let start = i;
            while i < n && src[i] != b'\n' {
                i += 1;
            }
            blank_range(&mut out, src, start, i);
            continue;
        }

        // Raw string with backticks (no escapes, can span lines)
        if c == b'`' {
            i += 1;
            while i < n && src[i] != b'`' {
                i += 1;
            }
            if i < n {
                i += 1;
            }
            continue;
        }

        // Interpreted string with double quotes
        if c == b'"' {
            i += 1;
            while i < n && src[i] != b'"' {
                if src[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if src[i] == b'\n' {
                    break;
                }
                i += 1;
            }
            if i < n && src[i] == b'"' {
                i += 1;
            }
            continue;
        }

        // Rune literal with single quotes (handle as a tiny string)
        if c == b'\'' {
            i += 1;
            while i < n && src[i] != b'\'' {
                if src[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if src[i] == b'\n' {
                    break;
                }
                i += 1;
            }
            if i < n && src[i] == b'\'' {
                i += 1;
            }
            continue;
        }

        i += 1;
    }

    finish(out)
}

// ---------- Rust ----------

fn strip_rust(source: &str) -> String {
    let src = source.as_bytes();
    let mut out = src.to_vec();
    let n = src.len();
    let mut i = 0usize;

    while i < n {
        let c = src[i];
        let c2 = at(src, i + 1);

        // Nested block comment /* ... /* ... */ ... */
        if c == b'/' && c2 == b'*' {
            let start = i;
            i += 2;
            let mut depth = 1u32;
            while i < n && depth > 0 {
                if src[i] == b'/' && at(src, i + 1) == b'*' {
                    depth += 1;
                    i += 2;
                } else if src[i] == b'*' && at(src, i + 1) == b'/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            blank_range(&mut out, src, start, i.min(n));
            continue;
        }

        // Line comment
        if c == b'/' && c2 == b'/' {
            let start = i;
            while i < n && src[i] != b'\n' {
                i += 1;
            }
            blank_range(&mut out, src, start, i);
            continue;
        }

        // String literals
        if c == b'"' {
            i += 1;
            while i < n && src[i] != b'"' {
                if src[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                i += 1;
            }
            if i < n && src[i] == b'"' {
                i += 1;
            }
            continue;
        }

        // Char literal — keep simple: skip 'x' or '\x'
        if c == b'\'' {
            // Could be a lifetime, e.g. 'a, but those don't contain routing text
            i += 1;
            while i < n && src[i] != b'\'' {
                if src[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if src[i] == b'\n' {
                    break;
                }
                i += 1;
            }
            if i < n && src[i] == b'\'' {
                i += 1;
            }
            continue;
        }

        i += 1;
    }

    finish(out)
}

#[cfg(test)]
mod tests {
    //! Port of `__tests__/strip-comments.test.ts` (every case).
    use super::*;

    #[test]
    fn python_strips_line_comments() {
        let src = "x = 1  # path('/fake/', View)\nreal = 2";
        let out = strip_comments_for_regex(src, CommentLang::Python);
        assert!(!out.contains("path('/fake/"));
        assert!(out.contains("real = 2"));
    }

    #[test]
    fn python_strips_triple_quoted_docstrings() {
        let src = "\"\"\"\npath('/in-docstring/', View)\n\"\"\"\nreal = 1\n";
        let out = strip_comments_for_regex(src, CommentLang::Python);
        assert!(!out.contains("in-docstring"));
        assert!(out.contains("real = 1"));
    }

    #[test]
    fn python_keeps_hash_inside_strings() {
        let src = "path('#/fragment/', View)\n";
        let out = strip_comments_for_regex(src, CommentLang::Python);
        assert!(out.contains("'#/fragment/'"));
    }

    #[test]
    fn python_handles_triple_single_quoted_docstrings() {
        let src = "'''\npath('/fake/')\n'''\nreal = 1\n";
        let out = strip_comments_for_regex(src, CommentLang::Python);
        assert!(!out.contains("fake"));
        assert!(out.contains("real = 1"));
    }

    #[test]
    fn typescript_strips_line_and_block_comments() {
        let src = "// app.get('/fake', x)\n/* app.get('/also-fake', y) */\napp.get('/real', z)";
        let out = strip_comments_for_regex(src, CommentLang::Typescript);
        assert!(!out.contains("fake"));
        assert!(out.contains("'/real'"));
    }

    #[test]
    fn typescript_keeps_double_slash_inside_strings() {
        let src = "const url = \"https://example.com/path\";\n";
        let out = strip_comments_for_regex(src, CommentLang::Typescript);
        assert!(out.contains("https://example.com/path"));
    }

    #[test]
    fn php_strips_slash_hash_and_block_comments() {
        let src = "// Route::get('/a', X::class)\n# Route::get('/b', Y::class)\n/* Route::get('/c', Z::class) */\nReal::go();";
        let out = strip_comments_for_regex(src, CommentLang::Php);
        assert!(!out.contains("'/a'"));
        assert!(!out.contains("'/b'"));
        assert!(!out.contains("'/c'"));
        assert!(out.contains("Real::go();"));
    }

    #[test]
    fn ruby_strips_begin_end_blocks() {
        let src = "=begin\nget '/fake', to: 'x#y'\n=end\nget '/real', to: 'a#b'\n";
        let out = strip_comments_for_regex(src, CommentLang::Ruby);
        assert!(!out.contains("fake"));
        assert!(out.contains("'/real'"));
    }

    #[test]
    fn ruby_strips_hash_comments() {
        let src = "# get '/fake', to: 'x#y'\nget '/real', to: 'a#b'\n";
        let out = strip_comments_for_regex(src, CommentLang::Ruby);
        assert!(!out.contains("fake"));
        assert!(out.contains("'/real'"));
    }

    #[test]
    fn rust_handles_nested_block_comments() {
        let src = "/* outer /* inner */ still in outer */ .route(\"/real\", get(h))";
        let out = strip_comments_for_regex(src, CommentLang::Rust);
        assert!(!out.contains("inner"));
        assert!(out.contains("/real"));
    }

    #[test]
    fn go_keeps_backtick_raw_strings_strips_line_comments() {
        let src = "// r.GET(\"/fake\", h)\nr.GET(`/real`, h2)\n";
        let out = strip_comments_for_regex(src, CommentLang::Go);
        assert!(!out.contains("fake"));
        // backtick raw string contents preserved
        assert!(out.contains("`/real`"));
    }

    #[test]
    fn go_strips_block_comments_containing_route_shaped_text() {
        let src = "/* r.GET(\"/fake\", h) */\nr.GET(\"/real\", h2)\n";
        let out = strip_comments_for_regex(src, CommentLang::Go);
        assert!(!out.contains("fake"));
        assert!(out.contains("\"/real\""));
    }

    #[test]
    fn java_strips_line_and_block_comments() {
        let src = "// @GetMapping(\"/fake\")\n/* @PostMapping(\"/also-fake\") */\n@GetMapping(\"/real\")\n";
        let out = strip_comments_for_regex(src, CommentLang::Java);
        assert!(!out.contains("fake"));
        assert!(out.contains("\"/real\""));
    }

    #[test]
    fn csharp_strips_line_and_block_comments() {
        let src =
            "// [HttpGet(\"/fake\")]\n/* [HttpPost(\"/also-fake\")] */\n[HttpGet(\"/real\")]\n";
        let out = strip_comments_for_regex(src, CommentLang::Csharp);
        assert!(!out.contains("fake"));
        assert!(out.contains("\"/real\""));
    }

    #[test]
    fn swift_strips_line_and_block_comments() {
        let src = "// app.get(\"fake\", use: x)\n/* app.get(\"also-fake\", use: y) */\napp.get(\"real\", use: z)\n";
        let out = strip_comments_for_regex(src, CommentLang::Swift);
        assert!(!out.contains("fake"));
        assert!(out.contains("\"real\""));
    }

    #[test]
    fn preserves_line_numbers_newlines_retained() {
        let src = "line1\n# comment with path('/fake/')\nline3";
        let out = strip_comments_for_regex(src, CommentLang::Python);
        let lines: Vec<&str> = out.split('\n').collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[2], "line3");
    }

    #[test]
    fn preserves_overall_length_so_source_offsets_stay_valid() {
        let src = "x = 1  # path('/fake/', View)\nreal = 2";
        let out = strip_comments_for_regex(src, CommentLang::Python);
        assert_eq!(out.len(), src.len());
    }

    // Rust-port-specific: multi-byte UTF-8 inside a stripped region must
    // blank to the same BYTE length (regex offsets are byte offsets) and
    // stay valid UTF-8.
    #[test]
    fn preserves_byte_length_with_multibyte_chars_in_comments() {
        let src = "x = 1  # 日本語 path('/fake/')\nreal = 2";
        let out = strip_comments_for_regex(src, CommentLang::Python);
        assert_eq!(out.len(), src.len()); // byte length
        assert!(!out.contains("日本語"));
        assert!(!out.contains("fake"));
        assert!(out.contains("real = 2"));
    }
}

//! Pattern compilation and line-oriented matching over a file's bytes.
//!
//! Patterns use the Rust `regex` syntax (the one ripgrep uses). The whole
//! buffer is searched at once, so the regex engine's literal prefilters do
//! the heavy lifting, but a hit is always a *line*: a match that runs past
//! the end of its line (`\s` and negated classes can cross a newline) only
//! counts when the line matches on its own.

use regex::bytes::{Regex, RegexBuilder};

/// How a `pattern` argument is read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::mcp::tools) struct PatternFlags {
    /// Match the pattern as a fixed string, not a regex.
    pub literal: bool,
    pub case_insensitive: bool,
    /// Match whole words only (grep's `-w`): the match may not touch a word
    /// character on either side.
    pub word: bool,
}

/// Compile `pattern` for line-oriented matching (`^`/`$` anchor at line
/// boundaries, CRLF-aware). A regex error is returned as its message.
pub(in crate::mcp::tools) fn compile(
    pattern: &str,
    flags: PatternFlags,
) -> std::result::Result<Regex, String> {
    let source = if flags.literal {
        regex::escape(pattern)
    } else {
        grep_alternation(pattern)
    };
    // ripgrep's `-w`: a non-word character or a line edge on both sides, so
    // it also works for patterns that begin or end with punctuation.
    let source = if flags.word {
        format!(r"(?:^|[^\w])(?:{source})(?:[^\w]|$)")
    } else {
        source
    };
    RegexBuilder::new(&source)
        .multi_line(true)
        .crlf(true)
        .case_insensitive(flags.case_insensitive)
        .size_limit(16 << 20)
        .dfa_size_limit(32 << 20)
        .build()
        .map_err(|error| error.to_string())
}

/// grep's basic syntax writes alternation as `\|`; in Rust regex syntax that
/// is a literal pipe, which nobody searching code means. Rewrite it to `|`
/// (a literal pipe is still `[|]`). An escaped backslash (`\\|`) is left
/// alone: that is a literal backslash followed by an alternation.
fn grep_alternation(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('|') => out.push('|'),
            Some(next) => {
                out.push('\\');
                out.push(next);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// One matching line of a buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::mcp::tools) struct LineMatch {
    /// 1-based line number.
    pub line: u32,
    /// Byte range of the line, without its terminator (`\n` or `\r\n`).
    pub start: usize,
    pub end: usize,
    /// Byte offset of the first match, relative to `start`.
    pub column: usize,
}

/// Call `on_line` for every line of `buf` that `regex` matches, in order,
/// once per line. Stops early when `on_line` returns `false`.
pub(in crate::mcp::tools) fn for_each_matching_line(
    regex: &Regex,
    buf: &[u8],
    mut on_line: impl FnMut(LineMatch) -> bool,
) {
    let mut pos = 0usize;
    // Line number of `counted_to`, the offset line counting has reached.
    let mut line = 1u32;
    let mut counted_to = 0usize;
    while pos <= buf.len() {
        let Some(found) = regex.find_at(buf, pos) else {
            return;
        };
        // An empty match after the final newline is not a line.
        if found.start() == buf.len() && (buf.is_empty() || buf.ends_with(b"\n")) {
            return;
        }
        let line_start = buf[..found.start()]
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |i| i + 1);
        let line_end = buf[found.start()..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(buf.len(), |i| found.start() + i);
        // Where the next search starts: past this line's terminator.
        let next = line_end + 1;
        let column = if found.end() <= line_end {
            Some(found.start() - line_start)
        } else {
            // The match crossed into the next line; the line alone decides.
            regex
                .find(&buf[line_start..line_end])
                .map(|local| local.start())
        };
        if let Some(column) = column {
            line += count_newlines(&buf[counted_to..line_start]);
            counted_to = line_start;
            let text_end = if line_end > line_start && buf[line_end - 1] == b'\r' {
                line_end - 1
            } else {
                line_end
            };
            let keep_going = on_line(LineMatch {
                line,
                start: line_start,
                end: text_end,
                column: column.min(text_end - line_start),
            });
            if !keep_going {
                return;
            }
        }
        if next > buf.len() {
            return;
        }
        pos = next;
    }
}

fn count_newlines(bytes: &[u8]) -> u32 {
    bytes.iter().filter(|&&b| b == b'\n').count() as u32
}

/// Whether a buffer looks binary: a NUL byte near the start (ripgrep's
/// heuristic).
pub(in crate::mcp::tools) fn looks_binary(buf: &[u8]) -> bool {
    buf[..buf.len().min(8192)].contains(&0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(pattern: &str, flags: PatternFlags, text: &str) -> Vec<(u32, String, usize)> {
        let regex = compile(pattern, flags).unwrap();
        let mut out = Vec::new();
        for_each_matching_line(&regex, text.as_bytes(), |m| {
            out.push((
                m.line,
                String::from_utf8_lossy(&text.as_bytes()[m.start..m.end]).into_owned(),
                m.column,
            ));
            true
        });
        out
    }

    #[test]
    fn reports_each_matching_line_once_with_its_number_and_column() {
        let text = "alpha\nbeta beta\n\ngamma beta\n";
        assert_eq!(
            lines("beta", PatternFlags::default(), text),
            vec![(2, "beta beta".into(), 0), (4, "gamma beta".into(), 6)]
        );
    }

    #[test]
    fn anchors_bind_to_lines_and_crlf_is_stripped() {
        let text = "fn a() {}\r\n  fn b() {}\r\nfn c() {}";
        let found = lines(r"^fn \w+", PatternFlags::default(), text);
        assert_eq!(
            found,
            vec![(1, "fn a() {}".into(), 0), (3, "fn c() {}".into(), 0)]
        );
        assert_eq!(
            lines(r"\{\}$", PatternFlags::default(), text).len(),
            3,
            "`$` must match before a CRLF"
        );
    }

    #[test]
    fn a_match_never_spans_lines() {
        // `\s+` would happily eat the newline between `foo` and `bar`.
        let text = "foo\nbar\nfoo bar\n";
        assert_eq!(
            lines(r"foo\s+bar", PatternFlags::default(), text),
            vec![(3, "foo bar".into(), 0)]
        );
        // A crossing match on a line that also matches locally still counts.
        let text = "x foo bar foo\nbar\n";
        assert_eq!(
            lines(r"foo\s+bar", PatternFlags::default(), text),
            vec![(1, "x foo bar foo".into(), 2)]
        );
    }

    #[test]
    fn literal_mode_escapes_metacharacters() {
        let flags = PatternFlags {
            literal: true,
            ..Default::default()
        };
        let text = "a.b(c)\naxb(c)\n";
        assert_eq!(lines("a.b(c)", flags, text), vec![(1, "a.b(c)".into(), 0)]);
    }

    #[test]
    fn grep_basic_alternation_is_accepted() {
        let text = "one\ntwo\nthree|four\n";
        assert_eq!(
            lines(r"one\|two", PatternFlags::default(), text)
                .iter()
                .map(|l| l.0)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        // `[|]` still means a literal pipe.
        assert_eq!(
            lines(r"three[|]four", PatternFlags::default(), text).len(),
            1
        );
        assert_eq!(grep_alternation(r"a\\|b"), r"a\\|b");
        assert_eq!(grep_alternation(r"a\(b\)"), r"a\(b\)");
    }

    #[test]
    fn word_mode_matches_whole_words_only() {
        let flags = PatternFlags {
            word: true,
            ..Default::default()
        };
        let text = "let id = 1;\nlet idx = 2;\nuid(id)\n";
        let found: Vec<u32> = lines("id", flags, text).iter().map(|l| l.0).collect();
        assert_eq!(found, vec![1, 3]);
        let literal = PatternFlags {
            word: true,
            literal: true,
            ..Default::default()
        };
        assert_eq!(lines("-v", literal, "run -v now\nrun -vv\n").len(), 1);
        assert_eq!(lines("a|b", flags, "a\nab\nb c\n").len(), 2);
    }

    #[test]
    fn case_insensitive_flag_applies() {
        let flags = PatternFlags {
            case_insensitive: true,
            ..Default::default()
        };
        assert_eq!(lines("todo", flags, "// TODO: x\n").len(), 1);
        assert!(lines("todo", PatternFlags::default(), "// TODO: x\n").is_empty());
    }

    #[test]
    fn empty_matches_and_last_line_without_newline() {
        assert_eq!(lines("^$", PatternFlags::default(), "a\n\nb").len(), 1);
        assert_eq!(lines("^$", PatternFlags::default(), "a\n\nb\n").len(), 1);
        assert!(lines("^", PatternFlags::default(), "").is_empty());
        assert_eq!(
            lines("end", PatternFlags::default(), "x\nthe end"),
            vec![(2, "the end".into(), 4)]
        );
    }

    #[test]
    fn invalid_patterns_report_the_regex_error() {
        let error = compile("foo(", PatternFlags::default()).unwrap_err();
        assert!(error.contains("unclosed group"), "{error}");
    }

    #[test]
    fn binary_detection_looks_for_nul() {
        assert!(looks_binary(b"ab\0cd"));
        assert!(!looks_binary(b"plain text"));
    }
}

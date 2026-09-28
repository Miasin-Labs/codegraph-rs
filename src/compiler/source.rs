//! One document's source text, as the compiler layer reads it: lines by
//! zero-based index, occurrence text by SCIP range, and the few syntactic
//! facts about an occurrence's surroundings that decide what kind of edge
//! it is (a call, a path qualifier, part of a `use` item, a comment).
//!
//! Everything is computed once per file (line starts, `use` spans); each
//! question about an occurrence is then O(1) or O(line), never O(file).

use super::scip::Range;

pub(crate) struct SourceText<'a> {
    text: &'a str,
    starts: Vec<usize>,
    /// Zero-based lines inside a `use` item.
    use_lines: Vec<bool>,
}

/// What follows an occurrence (whitespace skipped).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Follow {
    /// `(`: a call or a tuple constructor.
    Call,
    /// `{`: a struct literal (or a block after a type).
    Brace,
    /// `::`: a qualifier of the next path segment.
    PathSep,
    /// `!`: a macro invocation.
    Bang,
    Other,
}

impl<'a> SourceText<'a> {
    pub(crate) fn new(text: &'a str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            text.bytes()
                .enumerate()
                .filter(|(_, byte)| *byte == b'\n')
                .map(|(at, _)| at + 1),
        );
        let mut source = SourceText {
            text,
            starts,
            use_lines: Vec::new(),
        };
        source.use_lines = source.find_use_lines();
        source
    }

    pub(crate) fn line_count(&self) -> usize {
        self.starts.len()
    }

    /// Zero-based line `line`, without its newline.
    pub(crate) fn line(&self, line: u32) -> &'a str {
        let line = line as usize;
        let Some(&start) = self.starts.get(line) else {
            return "";
        };
        let end = self
            .starts
            .get(line + 1)
            .map_or(self.text.len(), |next| next - 1);
        let text = self.text.get(start..end.max(start)).unwrap_or("");
        text.strip_suffix('\r').unwrap_or(text)
    }

    /// The text a single-line range covers.
    pub(crate) fn slice(&self, range: &Range) -> &'a str {
        if range.start_line != range.end_line {
            return "";
        }
        self.line(range.start_line)
            .get(range.start_col as usize..range.end_col as usize)
            .unwrap_or("")
    }

    /// What comes after `range` on its line (then the next lines).
    pub(crate) fn follow(&self, range: &Range) -> Follow {
        let mut line = range.end_line;
        let mut col = range.end_col as usize;
        // Look at most a few lines ahead: `Foo\n    ::new()` is rare.
        for _ in 0..3 {
            let text = self.line(line);
            let mut rest = text.get(col..).unwrap_or("").trim_start();
            // `name::<T>…`: judge by what follows the generic arguments.
            if let Some(generic) = rest.strip_prefix("::<") {
                rest = after_generic(generic).unwrap_or("").trim_start();
            }
            if let Some(c) = rest.chars().next() {
                return match c {
                    '(' => Follow::Call,
                    '{' => Follow::Brace,
                    '!' => Follow::Bang,
                    ':' if rest.starts_with("::") => Follow::PathSep,
                    _ => Follow::Other,
                };
            }
            line += 1;
            col = 0;
            if line as usize >= self.line_count() {
                break;
            }
        }
        Follow::Other
    }

    /// In a pattern, read from its line: a match arm (`=> ` follows, or
    /// the line continues an arm with `|`), a `let`/`if let`/`while let`
    /// binding, or `matches!(`. `Some(x) =>` destructures; it does not
    /// construct.
    pub(crate) fn in_pattern(&self, range: &Range) -> bool {
        let line = self.line(range.start_line);
        let before = line.get(..range.start_col as usize).unwrap_or("");
        let after = line.get(range.end_col as usize..).unwrap_or("");
        if after.contains("=>") || line.trim_start().starts_with('|') {
            return true;
        }
        if before.contains("matches!(") {
            return true;
        }
        let binds = ["let ", "if let ", "while let "]
            .iter()
            .any(|keyword| before.contains(keyword));
        binds && after.contains('=') && !after.contains("==")
    }

    /// Preceded by `.` (a method call or field access).
    pub(crate) fn after_dot(&self, range: &Range) -> bool {
        let text = self.line(range.start_line);
        text.get(..range.start_col as usize)
            .is_some_and(|before| before.trim_end().ends_with('.'))
    }

    /// Inside a `use` item.
    pub(crate) fn in_use_item(&self, line: u32) -> bool {
        self.use_lines.get(line as usize).copied().unwrap_or(false)
    }

    /// On a comment or attribute line, or after `//` on its line.
    pub(crate) fn in_comment_or_attribute(&self, range: &Range) -> bool {
        let text = self.line(range.start_line);
        let trimmed = text.trim_start();
        if trimmed.starts_with("#[") || trimmed.starts_with("#![") || trimmed.starts_with("//") {
            return true;
        }
        text.get(..range.start_col as usize)
            .is_some_and(|before| before.contains("//"))
    }

    /// A macro invocation starts at `(line, col)`: `name!` or `a::name!`.
    pub(crate) fn macro_invocation_at(&self, line: u32, col: u32) -> bool {
        let text = self.line(line);
        let Some(rest) = text.get(col as usize..) else {
            return false;
        };
        let path_end = rest
            .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':'))
            .unwrap_or(rest.len());
        path_end > 0 && rest[path_end..].trim_start().starts_with('!')
    }

    fn find_use_lines(&self) -> Vec<bool> {
        let mut lines = vec![false; self.line_count()];
        let mut open = false;
        for (index, flag) in lines.iter_mut().enumerate() {
            let text = self.line(index as u32).trim_start();
            if !open && starts_use_item(text) {
                open = true;
            }
            if open {
                *flag = true;
                if text.contains(';') {
                    open = false;
                }
            }
        }
        lines
    }
}

/// The text after the `>` closing generic arguments whose `<` came just
/// before `text`.
fn after_generic(text: &str) -> Option<&str> {
    let mut depth = 1usize;
    for (at, c) in text.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[at + 1..]);
                }
            }
            _ => {}
        }
    }
    None
}

/// `use …`, `pub use …`, `pub(crate) use …`, `pub(in a::b) use …`.
fn starts_use_item(text: &str) -> bool {
    let rest = match text.strip_prefix("pub") {
        Some(after) => {
            let after = after.trim_start();
            match after.strip_prefix('(') {
                Some(inner) => inner.find(')').map_or("", |close| &inner[close + 1..]),
                None => after,
            }
        }
        None => text,
    };
    rest.trim_start()
        .strip_prefix("use")
        .is_some_and(|after| after.starts_with([' ', '\t', '{', ':']))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(line: u32, start: u32, end: u32) -> Range {
        Range {
            start_line: line,
            start_col: start,
            end_line: line,
            end_col: end,
        }
    }

    #[test]
    fn slices_and_follows() {
        let source = SourceText::new(
            "use a::{\n    b,\n};\nfn f() {\n    Foo::new(); x.bar(1); S { a: 1 }; m!(); g::<u8>();\n    V::<T>::new();\n}\n",
        );
        assert!(source.in_use_item(0) && source.in_use_item(1) && source.in_use_item(2));
        assert!(!source.in_use_item(3));
        assert_eq!(source.slice(&range(4, 4, 7)), "Foo");
        assert_eq!(source.follow(&range(4, 4, 7)), Follow::PathSep);
        assert_eq!(source.follow(&range(4, 9, 12)), Follow::Call);
        assert!(source.after_dot(&range(4, 18, 21)));
        assert_eq!(source.follow(&range(4, 26, 27)), Follow::Brace);
        assert_eq!(source.follow(&range(4, 38, 39)), Follow::Bang);
        assert_eq!(source.follow(&range(4, 44, 45)), Follow::Call);
        assert!(source.macro_invocation_at(4, 38));
        assert!(!source.macro_invocation_at(4, 4));
        let patterns = SourceText::new(
            "Some(x) => 1,\n| Kind::A(y) => 2,\nif let Kind::B(z) = v {\nlet w = Kind::C(1);\n",
        );
        assert!(patterns.in_pattern(&range(0, 0, 4)));
        assert!(patterns.in_pattern(&range(1, 8, 9)));
        assert!(patterns.in_pattern(&range(2, 13, 14)));
        assert!(!patterns.in_pattern(&range(3, 14, 15)));
        assert_eq!(source.follow(&range(5, 4, 5)), Follow::PathSep);
    }

    #[test]
    fn comments_attributes_and_use_forms() {
        let source =
            SourceText::new("#[derive(Debug)]\nlet x = 1; // see Foo\npub(crate) use x::Y;\n");
        assert!(source.in_comment_or_attribute(&range(0, 9, 14)));
        assert!(source.in_comment_or_attribute(&range(1, 18, 21)));
        assert!(!source.in_comment_or_attribute(&range(1, 4, 5)));
        assert!(source.in_use_item(2));
        assert!(!starts_use_item("user.name();"));
    }
}

//! One pass over a Markdown document's lines: its headings and fenced code
//! blocks, the only block structure the graph needs. Line-based and linear;
//! headings inside fences, HTML comments and YAML front matter are text.

/// An ATX (`## Title`) or setext (`Title` over `===`/`---`) heading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Heading {
    pub level: u8,
    /// The heading's text with inline markup removed.
    pub text: String,
    /// The line the heading starts on (1-based).
    pub line: u32,
    /// Byte offset where that line starts.
    pub byte: usize,
}

/// A fenced code block (```` ``` ```` or `~~~`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Fence {
    /// The info string after the opening fence, trimmed.
    pub info: String,
    /// The opening fence's line (1-based).
    pub open_line: u32,
    /// The closing fence's line, or the document's last line when the
    /// block runs to the end.
    pub close_line: u32,
    /// The first content line (1-based); `content` starts there.
    pub first_line: u32,
    /// Bytes of the content lines (opening/closing fences excluded).
    pub content: std::ops::Range<usize>,
    /// Columns of indentation of the opening fence (fences nested in list
    /// items), removed from each content line before parsing.
    pub indent: usize,
}

#[derive(Debug, Default)]
pub(super) struct Scan {
    pub headings: Vec<Heading>,
    pub fences: Vec<Fence>,
    /// How many lines `source.split('\n')` yields.
    pub line_count: u32,
    /// The last line of YAML front matter (`---` … `---` at line 1), or 0.
    pub front_matter_end: u32,
}

struct OpenFence {
    marker: u8,
    len: usize,
    indent: usize,
    info: String,
    open_line: u32,
    content_start: usize,
}

/// Leading spaces (a tab counts as 4) and the rest of the line.
fn split_indent(line: &str) -> (usize, &str) {
    let mut columns = 0;
    for (at, byte) in line.bytes().enumerate() {
        match byte {
            b' ' => columns += 1,
            b'\t' => columns += 4,
            _ => return (columns, &line[at..]),
        }
    }
    (columns, "")
}

/// `(marker, run length, rest)` when `text` opens or closes a fence.
fn fence_marker(text: &str) -> Option<(u8, usize, &str)> {
    let marker = *text.as_bytes().first()?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let len = text.bytes().take_while(|&b| b == marker).count();
    (len >= 3).then(|| (marker, len, &text[len..]))
}

/// The level and text of an ATX heading line (indent already removed).
fn atx_heading(text: &str) -> Option<(u8, &str)> {
    let level = text.bytes().take_while(|&b| b == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &text[level..];
    if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return None;
    }
    let mut body = rest.trim();
    // A closing sequence of `#`s preceded by a space is not content.
    let without_closing = body.trim_end_matches('#');
    if without_closing.len() < body.len()
        && (without_closing.is_empty() || without_closing.ends_with([' ', '\t']))
    {
        body = without_closing.trim_end();
    }
    Some((level as u8, body))
}

fn is_setext_underline(text: &str) -> Option<u8> {
    let trimmed = text.trim_end();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.bytes().all(|b| b == b'=') {
        return Some(1);
    }
    if trimmed.len() >= 2 && trimmed.bytes().all(|b| b == b'-') {
        return Some(2);
    }
    None
}

/// Whether a line can be the text of a setext heading: plain paragraph
/// text, not a list item, quote, table row, or another block.
fn is_paragraph_text(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    let first = text.as_bytes()[0];
    if matches!(first, b'>' | b'|' | b'<' | b'#') || fence_marker(text).is_some() {
        return false;
    }
    if (text.starts_with("- ") || text.starts_with("* ") || text.starts_with("+ "))
        || text.split_once(". ").is_some_and(|(n, _)| {
            !n.is_empty() && n.len() <= 3 && n.bytes().all(|b| b.is_ascii_digit())
        })
    {
        return false;
    }
    true
}

/// Remove inline Markdown from heading text: links keep their text, code
/// spans and emphasis lose their markers, `{#custom-id}` attributes go.
pub(super) fn plain_heading_text(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'`' | b'*' => at += 1,
            b'_' if at == 0
                || at + 1 == bytes.len()
                || !bytes[at - 1].is_ascii_alphanumeric()
                || !bytes[at + 1].is_ascii_alphanumeric() =>
            {
                at += 1;
            }
            b'!' if bytes.get(at + 1) == Some(&b'[') => at += 1,
            b']' if bytes.get(at + 1) == Some(&b'(') => {
                // `[text](url)`: keep the text, drop the target.
                match raw[at..].find(')') {
                    Some(close) => at += close + 1,
                    None => at += 1,
                }
            }
            b'[' => at += 1,
            b']' => at += 1,
            b'{' if raw[at..].starts_with("{#") && raw.trim_end().ends_with('}') => break,
            _ => {
                let ch = raw[at..].chars().next().expect("in bounds");
                out.push(ch);
                at += ch.len_utf8();
            }
        }
    }
    let collapsed: Vec<&str> = out.split_whitespace().collect();
    collapsed.join(" ")
}

/// GitHub's anchor for a heading: lowercase, punctuation dropped, spaces as
/// `-` (what `[x](#some-heading)` and `file.md#some-heading` name).
pub(crate) fn heading_slug(text: &str) -> String {
    let mut slug = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_alphanumeric() || ch == '-' || ch == '_' {
            slug.extend(ch.to_lowercase());
        } else if ch == ' ' {
            slug.push('-');
        }
    }
    slug
}

/// Scan the document once. `max_headings` bounds the headings kept; fences
/// are all recorded (their bodies are bounded by the caller).
pub(super) fn scan(source: &str, max_headings: usize) -> Scan {
    let mut scan = Scan::default();
    let mut open: Option<OpenFence> = None;
    let mut in_comment = false;
    let mut in_front_matter = false;
    // The previous line's text when it could be a setext heading's text.
    let mut previous: Option<(u32, usize, &str)> = None;
    let mut previous_blank = true;
    let mut offset = 0usize;
    let mut line_no = 0u32;

    for raw_line in source.split('\n') {
        line_no += 1;
        let line_start = offset;
        offset += raw_line.len() + 1;
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let (indent, text) = split_indent(line);

        if line_no == 1 && line.trim_end() == "---" {
            in_front_matter = true;
            continue;
        }
        if in_front_matter {
            if matches!(line.trim_end(), "---" | "...") {
                in_front_matter = false;
                scan.front_matter_end = line_no;
            }
            continue;
        }

        if let Some(fence) = &open {
            if let Some((marker, len, rest)) = fence_marker(text) {
                if marker == fence.marker && len >= fence.len && rest.trim().is_empty() {
                    let fence = open.take().expect("open fence");
                    scan.fences.push(Fence {
                        info: fence.info,
                        open_line: fence.open_line,
                        close_line: line_no,
                        first_line: fence.open_line + 1,
                        content: fence.content_start..line_start.max(fence.content_start),
                        indent: fence.indent,
                    });
                    previous = None;
                    previous_blank = true;
                }
            }
            continue;
        }

        if in_comment {
            if line.contains("-->") {
                in_comment = false;
            }
            previous = None;
            continue;
        }

        if let Some((marker, len, rest)) = fence_marker(text) {
            if marker == b'~' || !rest.contains('`') {
                open = Some(OpenFence {
                    marker,
                    len,
                    indent,
                    info: rest.trim().to_string(),
                    open_line: line_no,
                    content_start: offset.min(source.len()),
                });
                previous = None;
                continue;
            }
        }

        if text.starts_with("<!--") && !text.contains("-->") {
            in_comment = true;
            previous = None;
            continue;
        }

        if indent <= 3 {
            if let Some((level, body)) = atx_heading(text) {
                if scan.headings.len() < max_headings {
                    scan.headings.push(Heading {
                        level,
                        text: plain_heading_text(body),
                        line: line_no,
                        byte: line_start,
                    });
                }
                previous = None;
                previous_blank = false;
                continue;
            }
            if let (Some(level), Some((prev_line, prev_byte, prev_text))) =
                (is_setext_underline(text), previous)
            {
                if scan.headings.len() < max_headings {
                    scan.headings.push(Heading {
                        level,
                        text: plain_heading_text(prev_text),
                        line: prev_line,
                        byte: prev_byte,
                    });
                }
                previous = None;
                previous_blank = false;
                continue;
            }
        }

        let blank = text.is_empty();
        previous = (!blank && previous_blank && indent <= 3 && is_paragraph_text(text))
            .then_some((line_no, line_start, text.trim_end()));
        previous_blank = blank;
    }

    if let Some(fence) = open {
        // An unclosed fence runs to the end of the document.
        scan.fences.push(Fence {
            info: fence.info,
            open_line: fence.open_line,
            close_line: line_no,
            first_line: fence.open_line + 1,
            content: fence.content_start..source.len().max(fence.content_start),
            indent: fence.indent,
        });
    }
    scan.headings.retain(|heading| !heading.text.is_empty());
    scan.line_count = line_no;
    scan
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_skip_fences_comments_and_front_matter() {
        let source = "---\ntitle: x\n---\n# Title\n\ntext\n```rust\n# not a heading\n```\n<!--\n# hidden\n-->\nSetext\n------\n\n## Closing ##\n";
        let scan = scan(source, 100);
        let found: Vec<(u8, &str, u32)> = scan
            .headings
            .iter()
            .map(|h| (h.level, h.text.as_str(), h.line))
            .collect();
        assert_eq!(
            found,
            vec![(1, "Title", 4), (2, "Setext", 13), (2, "Closing", 16)]
        );
        assert_eq!(scan.front_matter_end, 3);
        assert_eq!(scan.fences.len(), 1);
        let fence = &scan.fences[0];
        assert_eq!(fence.info, "rust");
        assert_eq!(
            (fence.open_line, fence.first_line, fence.close_line),
            (7, 8, 9)
        );
        assert_eq!(&source[fence.content.clone()], "# not a heading\n");
    }

    #[test]
    fn list_items_and_rules_are_not_setext_headings() {
        let source = "- item\n---\n\ntext\n\n---\n";
        assert!(scan(source, 10).headings.is_empty());
    }

    #[test]
    fn heading_text_loses_inline_markup() {
        assert_eq!(
            plain_heading_text("The `Drop` trait and [RFC 1238](text/1238.md) {#drop}"),
            "The Drop trait and RFC 1238"
        );
        assert_eq!(plain_heading_text("snake_case **bold**"), "snake_case bold");
        assert_eq!(
            heading_slug("Drop check: `may_dangle`"),
            "drop-check-may_dangle"
        );
    }

    #[test]
    fn unclosed_fence_runs_to_the_end() {
        let source = "# A\n~~~\nfn x() {}\n# B\n";
        let scan = scan(source, 10);
        assert_eq!(scan.headings.len(), 1);
        assert_eq!(scan.fences[0].close_line, 5);
        assert_eq!(&source[scan.fences[0].content.clone()], "fn x() {}\n# B\n");
    }
}

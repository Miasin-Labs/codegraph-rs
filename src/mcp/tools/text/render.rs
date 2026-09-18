//! Hit lines the way agents read them: `N: text`, grouped under the symbol
//! they sit in, optionally with numbered context (`N- text`, grep's
//! convention) around each hit.
//!
//! Without context a hit is its trimmed line (still an exact substring of
//! the file). With context every line is verbatim — indentation kept, so a
//! block can be quoted back into an edit — and a window snaps to the whole
//! enclosing definition when that is barely larger than the window.

use std::collections::BTreeSet;

use regex::bytes::Regex;

use super::output::GrepHitGroup;
use super::scan::{Hit, MAX_HIT_CHARS};

/// Longest definition a context window snaps to.
const MAX_SNAP_LINES: u32 = 80;

/// The innermost indexed definition around a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::mcp::tools) struct Enclosing {
    pub name: String,
    pub start: u32,
    pub end: u32,
}

/// What a page needs beyond the scan: symbols from the index and current
/// file contents.
pub(in crate::mcp::tools) trait PageSource {
    fn enclosing(&mut self, path: &str, line: u32) -> Option<Enclosing>;
    fn read(&mut self, path: &str) -> Option<Vec<u8>>;
}

/// Context lines around each shown hit (grep's `-B`/`-A`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::mcp::tools) struct Window {
    pub before: u32,
    pub after: u32,
}

impl Window {
    pub fn is_empty(self) -> bool {
        self.before == 0 && self.after == 0
    }

    /// The lines to show around `line`: the whole enclosing definition when
    /// it is small (at most twice the window), else the window itself.
    fn around(self, line: u32, enclosing: Option<&Enclosing>, last_line: u32) -> (u32, u32) {
        let size = self.before + self.after + 1;
        if let Some(symbol) = enclosing {
            let span = symbol.end.saturating_sub(symbol.start) + 1;
            if span <= (2 * size).min(MAX_SNAP_LINES) && symbol.end <= last_line {
                return (symbol.start.max(1), symbol.end);
            }
        }
        (
            line.saturating_sub(self.before).max(1),
            line.saturating_add(self.after).min(last_line),
        )
    }
}

/// One chosen hit with its enclosing definition.
pub(in crate::mcp::tools) type Chosen<'h> = (&'h Hit, Option<Enclosing>);

/// Plain hits: one `N: text` line each (a bare `N` when `withhold`), grouped
/// by consecutive enclosing symbol.
pub(in crate::mcp::tools) fn plain(chosen: &[Chosen<'_>], withhold: bool) -> Vec<GrepHitGroup> {
    let mut groups: Vec<GrepHitGroup> = Vec::new();
    for (hit, enclosing) in chosen {
        let symbol = enclosing.as_ref().map(|symbol| symbol.name.clone());
        let line = if withhold {
            hit.line.to_string()
        } else {
            format!("{}: {}", hit.line, hit.text)
        };
        match groups.last_mut() {
            Some(group) if group.symbol == symbol => group.lines.push(line),
            _ => groups.push(GrepHitGroup {
                symbol,
                lines: vec![line],
            }),
        }
    }
    groups
}

/// Hits with context, from the file's current `content`. Windows that
/// overlap or touch merge into one block; every line of a block that
/// matches `regex` is marked `N:`, the rest `N-`. Returns the blocks and the
/// matching lines they show.
pub(in crate::mcp::tools) fn with_context(
    content: &[u8],
    chosen: &[Chosen<'_>],
    window: Window,
    regex: &Regex,
) -> (Vec<GrepHitGroup>, BTreeSet<u32>) {
    let lines: Vec<&[u8]> = content
        .strip_suffix(b"\n")
        .unwrap_or(content)
        .split(|&b| b == b'\n')
        .collect();
    let last_line = lines.len() as u32;
    let mut ranges: Vec<(u32, u32, Option<String>)> = chosen
        .iter()
        .filter(|(hit, _)| hit.line <= last_line)
        .map(|(hit, enclosing)| {
            let (start, end) = window.around(hit.line, enclosing.as_ref(), last_line);
            (
                start,
                end,
                enclosing.as_ref().map(|symbol| symbol.name.clone()),
            )
        })
        .collect();
    // A snapped definition can start before an earlier hit's window.
    ranges.sort_by_key(|(start, end, _)| (*start, *end));
    let mut blocks: Vec<(u32, u32, Option<String>)> = Vec::new();
    for (start, end, symbol) in ranges {
        match blocks.last_mut() {
            Some(block) if start <= block.1 + 1 => block.1 = block.1.max(end),
            _ => blocks.push((start, end, symbol)),
        }
    }
    let mut matched = BTreeSet::new();
    let groups = blocks
        .into_iter()
        .map(|(start, end, symbol)| GrepHitGroup {
            symbol,
            lines: (start..=end)
                .map(|number| {
                    let raw = lines[number as usize - 1];
                    let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
                    let is_match = regex.is_match(raw);
                    if is_match {
                        matched.insert(number);
                    }
                    let marker = if is_match { ':' } else { '-' };
                    let text = verbatim(raw);
                    if text.is_empty() {
                        format!("{number}{marker}")
                    } else {
                        format!("{number}{marker} {text}")
                    }
                })
                .collect(),
        })
        .collect();
    (groups, matched)
}

/// A context line as written (trailing blanks dropped), cut to
/// [`MAX_HIT_CHARS`] characters.
fn verbatim(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let text = text.trim_end();
    if text.chars().count() <= MAX_HIT_CHARS {
        return text.to_string();
    }
    let mut out: String = text.chars().take(MAX_HIT_CHARS - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::super::matcher::{PatternFlags, compile};
    use super::*;

    fn hit(line: u32) -> Hit {
        Hit {
            line,
            text: format!("hit {line}"),
            comment: false,
        }
    }

    fn enclosing(name: &str, start: u32, end: u32) -> Option<Enclosing> {
        Some(Enclosing {
            name: name.into(),
            start,
            end,
        })
    }

    #[test]
    fn plain_hits_are_numbered_and_grouped_by_symbol() {
        let (a, b, c) = (hit(3), hit(5), hit(9));
        let chosen = vec![
            (&a, enclosing("parse", 1, 6)),
            (&b, enclosing("parse", 1, 6)),
            (&c, None),
        ];
        let groups = plain(&chosen, false);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].symbol.as_deref(), Some("parse"));
        assert_eq!(groups[0].lines, vec!["3: hit 3", "5: hit 5"]);
        assert_eq!(groups[1].symbol, None);
        assert_eq!(plain(&chosen, true)[0].lines, vec!["3", "5"]);
    }

    #[test]
    fn context_windows_merge_mark_matches_and_keep_indentation() {
        let content = "fn a() {\n    let x = needle;\n    x\n}\n\nfn b() {}\nfn c() {}\n// needle\nfn d() {}\nfn e() {}\n";
        let regex = compile("needle", PatternFlags::default()).unwrap();
        let (h2, h8) = (hit(2), hit(8));
        let chosen = vec![(&h2, None), (&h8, None)];
        let window = Window {
            before: 1,
            after: 1,
        };
        let (groups, matched) = with_context(content.as_bytes(), &chosen, window, &regex);
        assert_eq!(groups.len(), 2);
        assert_eq!(
            groups[0].lines,
            vec!["1- fn a() {", "2:     let x = needle;", "3-     x"]
        );
        assert_eq!(
            groups[1].lines,
            vec!["7- fn c() {}", "8: // needle", "9- fn d() {}"]
        );
        assert_eq!(matched.into_iter().collect::<Vec<_>>(), vec![2, 8]);

        // Wider windows touch and merge into one block; blank lines stay numbered.
        let wide = Window {
            before: 3,
            after: 3,
        };
        let (groups, _) = with_context(content.as_bytes(), &chosen, wide, &regex);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].lines.first().unwrap(), "1- fn a() {");
        assert_eq!(groups[0].lines[4], "5-");
        assert_eq!(groups[0].lines.last().unwrap(), "10- fn e() {}");
    }

    #[test]
    fn a_small_enclosing_definition_is_shown_whole() {
        let content = (1..=40)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let regex = compile("line 12$", PatternFlags::default()).unwrap();
        let h = hit(12);
        let window = Window {
            before: 1,
            after: 3,
        };
        // A 9-line definition (<= 2 x 5) is shown whole ...
        let snapped = vec![(&h, enclosing("small", 10, 18))];
        let (groups, _) = with_context(content.as_bytes(), &snapped, window, &regex);
        assert_eq!(groups[0].lines.len(), 9);
        assert!(groups[0].lines[0].starts_with("10- "));
        assert_eq!(groups[0].symbol.as_deref(), Some("small"));
        // ... a 30-line one is not.
        let big = vec![(&h, enclosing("big", 1, 30))];
        let (groups, _) = with_context(content.as_bytes(), &big, window, &regex);
        assert_eq!(
            groups[0].lines,
            vec![
                "11- line 11",
                "12: line 12",
                "13- line 13",
                "14- line 14",
                "15- line 15"
            ]
        );
    }
}

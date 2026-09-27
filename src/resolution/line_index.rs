//! A file's lines, for work run once per reference or per node.
//!
//! Resolution visits the same source again and again (Rust receiver
//! inference per reference, the callback synthesizers per function), so each
//! thread keeps the line boundaries of the files it split last (keyed by the
//! shared source it was handed, not by path, so an edited file is never read
//! with stale boundaries). Several, so references alternating between files
//! do not re-split them. Splitting the whole file per visit was quadratic on
//! bundled files.

use std::cell::RefCell;
use std::sync::Arc;

/// How many files a [`SourceMemo`] keeps.
const RECENT_FILES: usize = 8;

thread_local! {
    /// Where the lines of the sources split on this thread last start.
    static STARTS: RefCell<SourceMemo<Vec<usize>>> = const { RefCell::new(SourceMemo::new()) };
}

/// Something derived from a file's text (its line starts, its export
/// clauses, its package), kept for the few files a thread saw last. Keyed by
/// the shared source (`read_file_arc`), not the path, so an edited file is
/// never answered from stale text. Per-reference code declares one in a
/// `thread_local!` instead of re-deriving it from the whole file each time.
pub(crate) struct SourceMemo<T> {
    recent: Vec<(Arc<str>, Arc<T>)>,
}

impl<T> SourceMemo<T> {
    pub(crate) const fn new() -> Self {
        Self { recent: Vec::new() }
    }

    pub(crate) fn get_or_insert_with(
        &mut self,
        source: &Arc<str>,
        derive: impl FnOnce(&str) -> T,
    ) -> Arc<T> {
        if let Some(at) = self
            .recent
            .iter()
            .position(|(seen, _)| Arc::ptr_eq(seen, source))
        {
            let entry = self.recent.remove(at);
            let value = Arc::clone(&entry.1);
            self.recent.push(entry);
            return value;
        }
        let value = Arc::new(derive(source));
        if self.recent.len() == RECENT_FILES {
            self.recent.remove(0);
        }
        self.recent.push((Arc::clone(source), Arc::clone(&value)));
        value
    }
}

/// The lines of a file by index, without collecting them: `get(i)` is the
/// `i`th item of `source.split('\n')` with a trailing `\r` removed.
pub(crate) struct Lines {
    source: Arc<str>,
    starts: Arc<Vec<usize>>,
}

impl Lines {
    pub(crate) fn of(source: &Arc<str>) -> Self {
        Self {
            starts: starts(source),
            source: Arc::clone(source),
        }
    }

    /// How many items `split('\n')` yields (at least 1).
    pub(crate) fn len(&self) -> usize {
        self.starts.len()
    }

    /// Never true (`split` yields at least one item); for clippy's pairing.
    pub(crate) fn is_empty(&self) -> bool {
        self.starts.is_empty()
    }

    /// How many lines `str::lines` yields (no final empty line after a
    /// trailing `\n`).
    pub(crate) fn str_lines_len(&self) -> usize {
        if self.source.is_empty() {
            0
        } else if self.source.ends_with('\n') {
            self.starts.len() - 1
        } else {
            self.starts.len()
        }
    }

    pub(crate) fn get(&self, index: usize) -> Option<&str> {
        let start = *self.starts.get(index)?;
        let end = self
            .starts
            .get(index + 1)
            .map_or(self.source.len(), |next| next - 1);
        let line = &self.source[start..end];
        Some(line.strip_suffix('\r').unwrap_or(line))
    }

    /// Lines `range` (clamped to the file) exactly as written, `\r`
    /// included: `source.split('\n').skip(start).take(len).join("\n")`
    /// without allocating (it is one contiguous slice of the source).
    pub(crate) fn raw_text(&self, range: std::ops::Range<usize>) -> &str {
        let end = range.end.min(self.len());
        let start = range.start.min(end);
        if start == end {
            return "";
        }
        let to = self
            .starts
            .get(end)
            .map_or(self.source.len(), |next| next - 1);
        &self.source[self.starts[start]..to]
    }

    /// Lines `range` (clamped to the file) joined by `\n`.
    pub(crate) fn join(&self, range: std::ops::Range<usize>) -> String {
        let end = range.end.min(self.len());
        let mut out = String::new();
        for index in range.start.min(end)..end {
            if index > range.start {
                out.push('\n');
            }
            out.push_str(self.get(index).unwrap_or(""));
        }
        out
    }

    pub(crate) fn iter(&self) -> impl DoubleEndedIterator<Item = &str> {
        self.span().iter()
    }

    /// Every line, as a span.
    pub(crate) fn span(&self) -> LineSpan<'_> {
        LineSpan {
            lines: self,
            start: 0,
            end: self.len(),
        }
    }
}

/// A borrowed run of a file's lines, `Copy` and free to slice — what
/// line-by-line scans take instead of a collected `&[&str]`.
#[derive(Clone, Copy)]
pub(crate) struct LineSpan<'a> {
    lines: &'a Lines,
    start: usize,
    end: usize,
}

impl<'a> LineSpan<'a> {
    pub(crate) fn len(&self) -> usize {
        self.end - self.start
    }

    pub(crate) fn get(&self, index: usize) -> Option<&'a str> {
        if index >= self.len() {
            return None;
        }
        self.lines.get(self.start + index)
    }

    /// Lines `range` of this span (clamped to it), indexed from 0 again.
    pub(crate) fn slice(&self, range: std::ops::Range<usize>) -> Self {
        let end = (self.start + range.end).min(self.end);
        let start = (self.start + range.start).min(end);
        Self {
            lines: self.lines,
            start,
            end,
        }
    }

    /// The lines from `from` to the end of the span.
    pub(crate) fn from(&self, from: usize) -> Self {
        self.slice(from..self.len())
    }

    pub(crate) fn iter(&self) -> impl DoubleEndedIterator<Item = &'a str> + 'a {
        let lines = self.lines;
        (self.start..self.end).filter_map(move |index| lines.get(index))
    }

    pub(crate) fn join(&self, separator: &str) -> String {
        let mut out = String::new();
        for (index, line) in self.iter().enumerate() {
            if index > 0 {
                out.push_str(separator);
            }
            out.push_str(line);
        }
        out
    }
}

/// Where each line of a text starts, for turning many byte offsets into
/// line numbers: build once per text, then each lookup is a binary search.
/// Counting newlines from the start per match (`line_of`) was quadratic in
/// matches × file size on bundled files.
pub(crate) struct LineStarts(Vec<usize>);

impl LineStarts {
    pub(crate) fn new(text: &str) -> Self {
        Self(
            std::iter::once(0)
                .chain(text.match_indices('\n').map(|(at, _)| at + 1))
                .collect(),
        )
    }

    /// The 1-based line of byte `offset`: `text[..offset].split('\n').count()`.
    pub(crate) fn line_of(&self, offset: usize) -> u32 {
        self.0.partition_point(|&start| start <= offset) as u32
    }

    /// How many `\n` precede byte `offset`.
    pub(crate) fn newlines_before(&self, offset: usize) -> u32 {
        self.line_of(offset) - 1
    }
}

/// Lines `start_line..=end_line` (1-based) joined by `\n`, exactly
/// `source.split('\n')` sliced and re-joined; `None` when a bound is 0.
pub(crate) fn slice_lines(source: &Arc<str>, start_line: u32, end_line: u32) -> Option<&str> {
    if start_line == 0 || end_line == 0 {
        return None;
    }
    let starts = starts(source);
    let start = ((start_line - 1) as usize).min(starts.len());
    let end = (end_line as usize).min(starts.len());
    if start >= end {
        return Some("");
    }
    let to = starts.get(end).map_or(source.len(), |next| next - 1);
    Some(&source[starts[start]..to])
}

fn starts(source: &Arc<str>) -> Arc<Vec<usize>> {
    STARTS.with(|memo| {
        memo.borrow_mut().get_or_insert_with(source, |text| {
            std::iter::once(0)
                .chain(text.match_indices('\n').map(|(at, _)| at + 1))
                .collect()
        })
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{LineStarts, Lines, slice_lines};

    #[test]
    fn splits_like_str_split_and_reuses_the_last_file() {
        let source: Arc<str> = Arc::from("fn a() {\r\n    b();\n}\n");
        let expected: Vec<&str> = source
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .collect();
        let first = Lines::of(&source);
        assert_eq!(first.iter().collect::<Vec<_>>(), expected);
        let again = Lines::of(&source);
        assert!(Arc::ptr_eq(&first.starts, &again.starts), "reused");
        let other: Arc<str> = Arc::from("x");
        assert_eq!(Lines::of(&other).iter().collect::<Vec<_>>(), ["x"]);
        // Alternating with another file keeps the first one's starts.
        assert!(Arc::ptr_eq(&first.starts, &Lines::of(&source).starts));
    }

    #[test]
    fn spans_slice_and_join_like_slices() {
        let source: Arc<str> = Arc::from("a\nb\nc\nd");
        let lines = Lines::of(&source);
        let span = lines.span();
        assert_eq!(span.slice(1..3).join("|"), "b|c");
        assert_eq!(span.slice(1..3).get(1), Some("c"));
        assert_eq!(span.slice(1..3).get(2), None);
        assert_eq!(span.from(3).iter().collect::<Vec<_>>(), ["d"]);
        assert_eq!(span.slice(2..99).len(), 2);
        assert_eq!(span.slice(9..12).len(), 0);
        assert_eq!(span.iter().next_back(), Some("d"));
    }

    #[test]
    fn slice_lines_matches_split_slice_join() {
        let source: Arc<str> = Arc::from("a\nb\r\nc\n\nd");
        let split: Vec<&str> = source.split('\n').collect();
        for start in 0..8u32 {
            for end in 0..8u32 {
                let expected = (start != 0 && end != 0).then(|| {
                    let from = ((start - 1) as usize).min(split.len());
                    let to = (end as usize).min(split.len());
                    if from >= to {
                        String::new()
                    } else {
                        split[from..to].join("\n")
                    }
                });
                assert_eq!(
                    slice_lines(&source, start, end).map(str::to_string),
                    expected
                );
            }
        }
    }

    #[test]
    fn lines_view_matches_split_and_str_lines() {
        for text in ["", "x", "a\r\nb\n", "a\n\nb\r\n\n", "\n"] {
            let source: Arc<str> = Arc::from(text);
            let view = Lines::of(&source);
            let split: Vec<&str> = text
                .split('\n')
                .map(|line| line.strip_suffix('\r').unwrap_or(line))
                .collect();
            assert_eq!(view.iter().collect::<Vec<_>>(), split, "{text:?}");
            assert_eq!(view.str_lines_len(), text.lines().count(), "{text:?}");
            assert_eq!(view.join(0..99), split.join("\n"), "{text:?}");
            let raw: Vec<&str> = text.split('\n').collect();
            for start in 0..raw.len() + 2 {
                for take in 0..4 {
                    let expected: Vec<&str> = raw.iter().skip(start).take(take).copied().collect();
                    assert_eq!(view.raw_text(start..start + take), expected.join("\n"));
                }
            }
        }
    }

    #[test]
    fn line_starts_count_like_splitting_the_prefix() {
        let text = "a\nbb\n\ncc\r\nd";
        let starts = LineStarts::new(text);
        for offset in 0..=text.len() {
            let expected = text[..offset].split('\n').count() as u32;
            assert_eq!(starts.line_of(offset), expected, "offset {offset}");
            assert_eq!(starts.newlines_before(offset), expected - 1);
        }
    }
}

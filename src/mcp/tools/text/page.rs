//! Turning a search outcome into one bounded, ranked page.
//!
//! Files are ranked (code before tests before docs before generated output,
//! files with a hit outside comments first, then by hit count). In `lines`
//! mode the first `limit` hits go to the best files, up to `maxPerFile` each
//! — within a file, lines outside test code and comments first — as
//! numbered `N: text` lines grouped under their enclosing symbol (with
//! context when asked). Further files are listed with their counts only, and
//! past the row cap the rest are summarized per directory. Every row says
//! how many of its matching lines were left out (`more`), and whatever the
//! page leaves out is the next page (`nextCursor`).

use std::collections::BTreeMap;

use regex::bytes::Regex;

use super::cursor::Cursor;
use super::output::{GrepDirRow, GrepFileRow, GrepOutput};
use super::rank::{FileClass, file_rank_key};
use super::render::{Chosen, PageSource, Window, plain, with_context};
use super::scan::{Candidate, FileHits, Hit, ScanOutcome};
use crate::extraction::is_value_sensitive_language;
use crate::search::is_test_symbol;

/// Directory rows summarizing the files past the row cap.
const MAX_DIRS: usize = 20;
/// Room kept for `nextCursor` and the flags added after fitting.
const CURSOR_ROOM: usize = 96;

/// What each file row carries (grep's default, `-c`, and `-l`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::mcp::tools) enum Mode {
    /// Numbered hit lines, and counts.
    #[default]
    Lines,
    /// Matching-line counts only.
    Count,
    /// File names only.
    Files,
}

impl Mode {
    pub fn parse(raw: Option<&str>) -> Option<Self> {
        match raw {
            None | Some("lines" | "content") => Some(Self::Lines),
            Some("count") => Some(Self::Count),
            Some("files" | "files_with_matches") => Some(Self::Files),
            Some(_) => None,
        }
    }

    /// Files listed per page.
    fn max_rows(self) -> usize {
        match self {
            Self::Lines => 40,
            Self::Count | Self::Files => 200,
        }
    }
}

pub(in crate::mcp::tools) struct Page<'a> {
    pub candidates: &'a [Candidate],
    pub cursor: Cursor,
    pub mode: Mode,
    /// Hits shown across the page.
    pub limit: usize,
    /// Hits shown per file.
    pub max_per_file: usize,
    /// Context lines around each shown hit.
    pub window: Window,
    /// The search, to mark the matching lines inside context blocks.
    pub regex: &'a Regex,
    pub budget: usize,
    /// Query identity the cursors carry.
    pub key: &'a str,
}

impl Page<'_> {
    pub fn assemble(
        &self,
        outcome: ScanOutcome,
        is_generated: impl Fn(&str) -> bool,
        source: &mut impl PageSource,
    ) -> GrepOutput {
        let total = self.candidates.len();
        let mut out = GrepOutput::new();
        out.skipped_files = outcome.too_large;
        out.incomplete = outcome.stopped.is_some();
        if self.cursor.start > 0 || outcome.end < total {
            out.searched_files = Some(outcome.end - self.cursor.start.min(outcome.end));
            out.candidate_files = Some(total);
        }
        if self.cursor.end.is_some() && outcome.stopped.is_some() {
            // A page handed out earlier must be searched in full again to
            // rank the same way; this attempt ran out, so offer it again.
            out.next_cursor = Some(self.cursor.encode(self.key));
            return out;
        }

        let mut ranked: Vec<(FileClass, FileHits)> = outcome
            .files
            .into_iter()
            .map(|hits| {
                let candidate = &self.candidates[hits.index];
                let generated = is_generated(&candidate.path);
                (
                    FileClass::of(&candidate.path, candidate.language, generated),
                    hits,
                )
            })
            .collect();
        ranked.sort_by(|(a_class, a), (b_class, b)| {
            let a_path = &self.candidates[a.index].path;
            let b_path = &self.candidates[b.index].path;
            file_rank_key(*a_class, a.code_hits, a.count, a_path).cmp(&file_rank_key(
                *b_class,
                b.code_hits,
                b.count,
                b_path,
            ))
        });
        let total_files = ranked.len();
        let total_hits: usize = ranked.iter().map(|(_, hits)| hits.count).sum();

        let skip = self.cursor.skip.min(ranked.len());
        let mut shown = 0usize;
        for (_, hits) in ranked[skip..].iter().take(self.mode.max_rows()) {
            let candidate = &self.candidates[hits.index];
            let row = match self.mode {
                Mode::Files => GrepFileRow {
                    file: candidate.path.clone(),
                    count: None,
                    more: 0,
                    hits: Vec::new(),
                    already_sent: Vec::new(),
                },
                Mode::Count => GrepFileRow {
                    file: candidate.path.clone(),
                    count: Some(hits.count),
                    more: 0,
                    hits: Vec::new(),
                    already_sent: Vec::new(),
                },
                Mode::Lines => {
                    let withhold = is_value_sensitive_language(candidate.language);
                    let room = self.limit.saturating_sub(shown).min(self.max_per_file);
                    let chosen = choose(&hits.hits, room, &candidate.path, source);
                    shown += chosen.len();
                    out.values_withheld |= withhold && !chosen.is_empty();
                    self.lines_row(candidate, hits, &chosen, withhold, source)
                }
            };
            out.files.push(row);
        }
        let listed = out.files.len();
        out.dirs = dir_summary(
            ranked[skip + listed..]
                .iter()
                .map(|(_, hits)| (self.candidates[hits.index].path.as_str(), hits.count)),
        );
        out.truncated = skip + listed < ranked.len();
        let dropped = out.fit_to(self.budget.saturating_sub(CURSOR_ROOM));
        let consumed = skip + listed - dropped;
        // The totals only say something the rows do not when rows are
        // missing: on another page, or past this one.
        if skip > 0 || consumed < ranked.len() {
            out.total_files = Some(total_files);
            out.total_hits = Some(total_hits);
        }
        out.next_cursor = if consumed < ranked.len() {
            Some(Cursor {
                start: self.cursor.start,
                end: Some(outcome.end),
                skip: consumed,
            })
        } else if outcome.end < total {
            Some(Cursor {
                start: outcome.end,
                end: None,
                skip: 0,
            })
        } else {
            None
        }
        .map(|cursor| cursor.encode(self.key));
        out
    }

    /// A `lines`-mode row: the chosen hits as numbered lines (in context
    /// blocks when a window is set), the lines the session holds, and how
    /// many matching lines neither shows.
    fn lines_row(
        &self,
        candidate: &Candidate,
        hits: &FileHits,
        chosen: &[Chosen<'_>],
        withhold: bool,
        source: &mut impl PageSource,
    ) -> GrepFileRow {
        let mut already_sent = hits.sent.clone();
        let mut sent_total = hits.sent_total;
        let context = (!withhold && !self.window.is_empty() && !chosen.is_empty())
            .then(|| source.read(&candidate.path))
            .flatten();
        let (groups, shown) = match context {
            Some(content) => {
                let (groups, matched) = with_context(&content, chosen, self.window, self.regex);
                // A block that shows a line the session holds shows it once.
                let listed = already_sent.len();
                already_sent.retain(|line| !matched.contains(line));
                sent_total = sent_total.saturating_sub(listed - already_sent.len());
                (groups, matched.len())
            }
            None => (plain(chosen, withhold), chosen.len()),
        };
        GrepFileRow {
            file: candidate.path.clone(),
            count: Some(hits.count),
            more: hits.count.saturating_sub(shown + sent_total),
            hits: groups,
            already_sent,
        }
    }
}

/// Pick up to `room` of a file's candidate lines to show: lines outside test
/// code (an inline `mod tests`, by the enclosing symbol) before test code,
/// code before comments, then line order; returned in line order with their
/// enclosing definitions.
fn choose<'h>(
    candidates: &'h [Hit],
    room: usize,
    path: &str,
    source: &mut impl PageSource,
) -> Vec<Chosen<'h>> {
    if room == 0 {
        return Vec::new();
    }
    let mut picked: Vec<Chosen<'h>> = candidates
        .iter()
        .map(|hit| (hit, source.enclosing(path, hit.line)))
        .collect();
    picked.sort_by_key(|(hit, enclosing)| {
        (
            enclosing
                .as_ref()
                .is_some_and(|symbol| is_test_symbol(path, &symbol.name)),
            hit.comment,
            hit.line,
        )
    });
    picked.truncate(room);
    picked.sort_by_key(|(hit, _)| hit.line);
    picked
}

/// Group `(path, count)` rows by directory, at the deepest level that needs
/// no more than [`MAX_DIRS`] rows, most hits first.
fn dir_summary<'a>(rows: impl Iterator<Item = (&'a str, usize)>) -> Vec<GrepDirRow> {
    let rows: Vec<(&str, usize)> = rows.collect();
    if rows.is_empty() {
        return Vec::new();
    }
    let dir_of = |path: &'a str| path.rsplit_once('/').map_or(".", |(dir, _)| dir);
    let deepest = rows
        .iter()
        .map(|(path, _)| dir_of(path).split('/').count())
        .max()
        .unwrap_or(1);
    let group = |depth: usize| {
        let mut groups: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        for (path, count) in &rows {
            let dir = dir_of(path);
            let key = dir.split('/').take(depth).collect::<Vec<_>>().join("/");
            let entry = groups.entry(key).or_default();
            entry.0 += 1;
            entry.1 += count;
        }
        groups
    };
    let mut depth = deepest;
    let mut groups = group(depth);
    while groups.len() > MAX_DIRS && depth > 1 {
        depth -= 1;
        groups = group(depth);
    }
    let mut dirs: Vec<GrepDirRow> = groups
        .into_iter()
        .map(|(dir, (files, count))| GrepDirRow { dir, files, count })
        .collect();
    dirs.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.dir.cmp(&b.dir)));
    dirs.truncate(MAX_DIRS);
    dirs
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::super::matcher::{PatternFlags, compile};
    use super::super::render::Enclosing;
    use super::super::scan::StopReason;
    use super::*;
    use crate::types::Language;

    /// Symbols and contents from memory.
    #[derive(Default)]
    struct FakeSource {
        symbols: HashMap<(String, u32), Enclosing>,
        contents: HashMap<String, String>,
    }

    impl FakeSource {
        fn symbol(mut self, path: &str, lines: std::ops::RangeInclusive<u32>, name: &str) -> Self {
            for line in lines.clone() {
                self.symbols.insert(
                    (path.to_string(), line),
                    Enclosing {
                        name: name.into(),
                        start: *lines.start(),
                        end: *lines.end(),
                    },
                );
            }
            self
        }
    }

    impl PageSource for FakeSource {
        fn enclosing(&mut self, path: &str, line: u32) -> Option<Enclosing> {
            self.symbols.get(&(path.to_string(), line)).cloned()
        }

        fn read(&mut self, path: &str) -> Option<Vec<u8>> {
            self.contents
                .get(path)
                .map(|text| text.clone().into_bytes())
        }
    }

    fn candidate(path: &str, language: Language) -> Candidate {
        Candidate {
            path: path.into(),
            language,
        }
    }

    fn hit(line: u32, comment: bool) -> Hit {
        Hit {
            line,
            text: format!("line {line}"),
            comment,
        }
    }

    fn hits(index: usize, count: usize, code_hits: usize, lines: &[u32]) -> FileHits {
        FileHits {
            index,
            count,
            code_hits,
            hits: lines.iter().map(|&line| hit(line, false)).collect(),
            sent: Vec::new(),
            sent_total: 0,
        }
    }

    fn outcome(files: Vec<FileHits>, end: usize) -> ScanOutcome {
        ScanOutcome {
            files,
            end,
            ..ScanOutcome::default()
        }
    }

    fn needle() -> Regex {
        compile("needle", PatternFlags::default()).unwrap()
    }

    fn page<'a>(candidates: &'a [Candidate], limit: usize, regex: &'a Regex) -> Page<'a> {
        Page {
            candidates,
            cursor: Cursor::FIRST,
            mode: Mode::Lines,
            limit,
            max_per_file: 3,
            window: Window::default(),
            regex,
            budget: 24_000,
            key: "k",
        }
    }

    fn all_lines(row: &GrepFileRow) -> Vec<String> {
        row.hits
            .iter()
            .flat_map(|group| group.lines.clone())
            .collect()
    }

    #[test]
    fn ranks_code_first_spends_the_hit_limit_and_names_symbols() {
        let regex = needle();
        let candidates = vec![
            candidate("README.md", Language::Markdown),
            candidate("config.toml", Language::Toml),
            candidate("src/a.rs", Language::Rust),
            candidate("src/b.rs", Language::Rust),
            candidate("tests/t.rs", Language::Rust),
        ];
        let mut source = FakeSource::default().symbol("src/b.rs", 1..=2, "f");
        let out = page(&candidates, 4, &regex).assemble(
            outcome(
                vec![
                    hits(0, 1, 1, &[3]),
                    hits(1, 1, 1, &[2]),
                    hits(2, 2, 2, &[5, 9]),
                    hits(3, 5, 5, &[1, 2, 3, 4, 5]),
                    hits(4, 9, 9, &[1, 2, 3]),
                ],
                5,
            ),
            |_| false,
            &mut source,
        );
        let order: Vec<&str> = out.files.iter().map(|f| f.file.as_str()).collect();
        assert_eq!(
            order,
            vec![
                "src/b.rs",
                "src/a.rs",
                "tests/t.rs",
                "README.md",
                "config.toml"
            ]
        );
        let b = &out.files[0];
        assert_eq!(b.hits.len(), 2, "grouped by enclosing symbol");
        assert_eq!(b.hits[0].symbol.as_deref(), Some("f"));
        assert_eq!(b.hits[0].lines, vec!["1: line 1", "2: line 2"]);
        assert_eq!(b.hits[1].symbol, None);
        assert_eq!(b.hits[1].lines, vec!["3: line 3"]);
        assert_eq!(all_lines(&out.files[1]), vec!["5: line 5"]);
        // Every row carries its count and says how many lines it left out.
        let counts: Vec<(Option<usize>, usize)> =
            out.files.iter().map(|f| (f.count, f.more)).collect();
        assert_eq!(
            counts,
            vec![
                (Some(5), 2),
                (Some(2), 1),
                (Some(9), 9),
                (Some(1), 1),
                (Some(1), 1)
            ]
        );
        // Every matching file is listed, so the totals would repeat the rows.
        assert_eq!((out.total_files, out.total_hits), (None, None));
        assert!(out.next_cursor.is_none() && !out.truncated && !out.incomplete);
        assert!(out.searched_files.is_none());
    }

    #[test]
    fn within_a_file_code_outside_tests_and_comments_goes_first() {
        let regex = needle();
        let candidates = vec![candidate("src/lib.rs", Language::Rust)];
        let file = FileHits {
            index: 0,
            count: 6,
            code_hits: 5,
            hits: vec![
                hit(1, true),
                hit(10, false),
                hit(11, false),
                hit(40, false),
                hit(50, false),
                hit(60, false),
            ],
            sent: Vec::new(),
            sent_total: 0,
        };
        let mut source = FakeSource::default()
            .symbol("src/lib.rs", 10..=39, "tests::parses")
            .symbol("src/lib.rs", 40..=70, "parse");
        let out =
            page(&candidates, 30, &regex).assemble(outcome(vec![file], 1), |_| false, &mut source);
        assert_eq!(
            all_lines(&out.files[0]),
            vec!["40: line 40", "50: line 50", "60: line 60"]
        );
        assert_eq!((out.files[0].count, out.files[0].more), (Some(6), 3));
    }

    #[test]
    fn context_blocks_come_from_the_file_and_count_their_matches() {
        let regex = needle();
        let candidates = vec![candidate("src/lib.rs", Language::Rust)];
        let body = "fn a() {\n    needle();\n    needle();\n}\nfn b() {}\n";
        let mut source = FakeSource::default();
        source
            .contents
            .insert("src/lib.rs".into(), body.to_string());
        let mut file = hits(0, 2, 2, &[2]);
        file.sent = vec![3];
        file.sent_total = 1;
        let out = Page {
            window: Window {
                before: 1,
                after: 1,
            },
            ..page(&candidates, 30, &regex)
        }
        .assemble(outcome(vec![file], 1), |_| false, &mut source);
        let row = &out.files[0];
        assert_eq!(
            all_lines(row),
            vec!["1- fn a() {", "2:     needle();", "3:     needle();"]
        );
        // Line 3 was already sent but the block shows it: listed once.
        assert!(row.already_sent.is_empty());
        assert_eq!(row.more, 0);
    }

    #[test]
    fn count_and_files_modes_list_rows_without_lines() {
        let regex = needle();
        let candidates = vec![
            candidate("src/a.rs", Language::Rust),
            candidate("src/b.rs", Language::Rust),
        ];
        let files = vec![hits(0, 2, 2, &[1, 2]), hits(1, 5, 5, &[1])];
        let mut source = FakeSource::default();
        let counts = Page {
            mode: Mode::Count,
            ..page(&candidates, 30, &regex)
        }
        .assemble(outcome(files.clone(), 2), |_| false, &mut source);
        let rows: Vec<(&str, Option<usize>, bool)> = counts
            .files
            .iter()
            .map(|f| (f.file.as_str(), f.count, f.hits.is_empty()))
            .collect();
        assert_eq!(
            rows,
            vec![("src/b.rs", Some(5), true), ("src/a.rs", Some(2), true)]
        );
        let names = Page {
            mode: Mode::Files,
            ..page(&candidates, 30, &regex)
        }
        .assemble(outcome(files, 2), |_| false, &mut source);
        assert!(
            names
                .files
                .iter()
                .all(|f| f.count.is_none() && f.hits.is_empty())
        );
        assert_eq!(Mode::parse(Some("files_with_matches")), Some(Mode::Files));
        assert_eq!(Mode::parse(Some("bogus")), None);
    }

    #[test]
    fn configuration_values_are_withheld() {
        let regex = needle();
        let candidates = vec![candidate("config.yaml", Language::Yaml)];
        let mut source = FakeSource::default().symbol("config.yaml", 4..=4, "db.password");
        let out = page(&candidates, 10, &regex).assemble(
            outcome(vec![hits(0, 1, 1, &[4])], 1),
            |_| false,
            &mut source,
        );
        assert!(out.values_withheld);
        let group = &out.files[0].hits[0];
        assert_eq!(group.lines, vec!["4"]);
        assert_eq!(group.symbol.as_deref(), Some("db.password"));
    }

    #[test]
    fn pages_past_the_row_cap_and_summarizes_the_rest_by_directory() {
        let regex = needle();
        let candidates: Vec<Candidate> = (0..100)
            .map(|i| candidate(&format!("src/d{}/f{i:03}.rs", i % 4), Language::Rust))
            .collect();
        let files: Vec<FileHits> = (0..100).map(|i| hits(i, 1, 1, &[1])).collect();
        let first_page = page(&candidates, 10, &regex);
        let mut source = FakeSource::default();
        let first = first_page.assemble(outcome(files.clone(), 100), |_| false, &mut source);
        let rows = Mode::Lines.max_rows();
        assert_eq!(first.files.len(), rows);
        assert!(first.truncated);
        assert_eq!(
            (first.total_files, first.total_hits),
            (Some(100), Some(100))
        );
        let summarized: usize = first.dirs.iter().map(|d| d.files).sum();
        assert_eq!(summarized, 100 - rows);
        assert!(first.dirs.iter().all(|d| d.dir.starts_with("src/d")));

        let next = Cursor::decode(first.next_cursor.as_deref().unwrap(), "k").unwrap();
        assert_eq!(
            next,
            Cursor {
                start: 0,
                end: Some(100),
                skip: rows
            }
        );
        let second = Page {
            cursor: next,
            ..first_page
        }
        .assemble(outcome(files, 100), |_| false, &mut source);
        assert_eq!(second.files.len(), rows);
        let overlap = second
            .files
            .iter()
            .filter(|row| first.files.iter().any(|seen| seen.file == row.file))
            .count();
        assert_eq!(overlap, 0, "pages never repeat a file");
    }

    #[test]
    fn an_incomplete_search_hands_out_a_cursor_that_resumes_after_it() {
        let regex = needle();
        let candidates: Vec<Candidate> = (0..10)
            .map(|i| candidate(&format!("f{i}.rs"), Language::Rust))
            .collect();
        let stopped = ScanOutcome {
            files: vec![hits(1, 2, 2, &[1, 2])],
            end: 4,
            stopped: Some(StopReason::Deadline),
            ..ScanOutcome::default()
        };
        let out =
            page(&candidates, 10, &regex).assemble(stopped, |_| false, &mut FakeSource::default());
        assert!(out.incomplete);
        assert_eq!(
            (out.searched_files, out.candidate_files),
            (Some(4), Some(10))
        );
        assert_eq!(
            Cursor::decode(out.next_cursor.as_deref().unwrap(), "k"),
            Some(Cursor {
                start: 4,
                end: None,
                skip: 0
            })
        );
    }

    #[test]
    fn a_re_searched_page_that_runs_out_offers_itself_again() {
        let regex = needle();
        let candidates: Vec<Candidate> = (0..10)
            .map(|i| candidate(&format!("f{i}.rs"), Language::Rust))
            .collect();
        let cursor = Cursor {
            start: 0,
            end: Some(10),
            skip: 5,
        };
        let stopped = ScanOutcome {
            files: vec![hits(1, 2, 2, &[1, 2])],
            end: 3,
            stopped: Some(StopReason::Deadline),
            ..ScanOutcome::default()
        };
        let out = Page {
            cursor,
            ..page(&candidates, 10, &regex)
        }
        .assemble(stopped, |_| false, &mut FakeSource::default());
        assert!(out.incomplete && out.files.is_empty());
        assert_eq!(
            Cursor::decode(out.next_cursor.as_deref().unwrap(), "k"),
            Some(cursor)
        );
    }

    #[test]
    fn the_budget_moves_trailing_rows_to_the_next_page() {
        let regex = needle();
        let candidates: Vec<Candidate> = (0..30)
            .map(|i| {
                candidate(
                    &format!("src/file_with_a_long_name_{i:02}.rs"),
                    Language::Rust,
                )
            })
            .collect();
        let files: Vec<FileHits> = (0..30).map(|i| hits(i, 1, 1, &[1])).collect();
        let out = Page {
            budget: 1_200,
            ..page(&candidates, 30, &regex)
        }
        .assemble(outcome(files, 30), |_| false, &mut FakeSource::default());
        assert!(out.truncated);
        let wire = serde_json::to_string(&out).unwrap();
        assert!(wire.len() <= 1_200, "{} > budget", wire.len());
        let next = Cursor::decode(out.next_cursor.as_deref().unwrap(), "k").unwrap();
        assert_eq!(next.skip, out.files.len());
    }

    #[test]
    fn dir_summary_collapses_to_fit() {
        let paths: Vec<String> = (0..60).map(|i| format!("a/b{i}/c/f.rs")).collect();
        let rows = paths.iter().map(|p| (p.as_str(), 2));
        let dirs = dir_summary(rows);
        assert_eq!(dirs.len(), 1);
        assert_eq!(
            (dirs[0].dir.as_str(), dirs[0].files, dirs[0].count),
            ("a", 60, 120)
        );
    }
}

//! Bounded, parallel search of the candidate files.
//!
//! Workers claim candidates in path order and stop claiming at the first of:
//! the wall-clock deadline, the byte budget, or client cancellation. The
//! outcome is always a prefix `start..end` of the candidates searched in
//! full — never a random subset — so a cursor can resume exactly where the
//! search stopped. The first candidate of a range is always searched, so
//! every page makes progress however tight the budgets are.

use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use regex::bytes::Regex;

use super::ledger::{SentLines, covers};
use super::matcher::{for_each_matching_line, looks_binary};
use super::open::Opener;
use super::rank::is_comment_line;
use crate::mcp::explore_session::content_fingerprint;
use crate::types::Language;

/// Longest hit text sent, in characters.
pub(in crate::mcp::tools) const MAX_HIT_CHARS: usize = 160;
/// Already-sent line numbers listed per file.
const MAX_SENT_LISTED: usize = 20;
/// Lines outside comments kept per file, as a multiple of `max_per_file`,
/// so the page can prefer lines outside test code once it knows the
/// enclosing symbols.
const CODE_CANDIDATES_PER_SHOWN: usize = 3;

/// One indexed file in scope.
#[derive(Debug, Clone)]
pub(in crate::mcp::tools) struct Candidate {
    pub path: String,
    pub language: Language,
}

/// What one search may spend.
#[derive(Debug, Clone, Copy)]
pub(in crate::mcp::tools) struct ScanLimits {
    pub deadline: Instant,
    /// Bytes read across all files.
    pub max_bytes: u64,
    /// Larger files are skipped (and counted).
    pub max_file_bytes: u64,
    pub threads: usize,
}

/// Why a search stopped before its range was covered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::mcp::tools) enum StopReason {
    Deadline,
    Bytes,
    Cancelled,
}

/// A hit line that may be shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::mcp::tools) struct Hit {
    pub line: u32,
    /// Trimmed and clipped to [`MAX_HIT_CHARS`] around the match.
    pub text: String,
    /// The line reads as a comment.
    pub comment: bool,
}

/// The matches of one candidate file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::mcp::tools) struct FileHits {
    /// Index into the candidates.
    pub index: usize,
    /// Matching lines.
    pub count: usize,
    /// Matching lines that are not comments.
    pub code_hits: usize,
    /// Lines the session does not hold yet that may be shown, in line
    /// order: the first few outside comments and the first few comments.
    pub hits: Vec<Hit>,
    /// Matching lines the session already holds (the first few).
    pub sent: Vec<u32>,
    /// All matching lines the session already holds.
    pub sent_total: usize,
}

pub(in crate::mcp::tools) struct ScanRequest<'a> {
    pub root: &'a Path,
    pub candidates: &'a [Candidate],
    pub start: usize,
    /// Search exactly `start..end`; `None` searches as far as the limits
    /// allow.
    pub end: Option<usize>,
    pub regex: &'a Regex,
    pub max_per_file: usize,
    pub sent: &'a SentLines,
    pub limits: ScanLimits,
    pub cancel: Option<&'a AtomicBool>,
}

#[derive(Debug, Default)]
pub(in crate::mcp::tools) struct ScanOutcome {
    /// Files with matches, in candidate order.
    pub files: Vec<FileHits>,
    /// Candidates `start..end` were searched.
    pub end: usize,
    pub stopped: Option<StopReason>,
    /// Searched files skipped for size.
    pub too_large: usize,
}

enum Slot {
    NotReached(StopReason),
    TooLarge,
    Nothing,
    Matched(FileHits),
}

pub(in crate::mcp::tools) fn scan(req: &ScanRequest<'_>) -> ScanOutcome {
    let end = req
        .end
        .unwrap_or(req.candidates.len())
        .min(req.candidates.len());
    let start = req.start.min(end);
    let total = end - start;
    let next = AtomicUsize::new(start);
    let bytes = AtomicU64::new(0);
    let stop = AtomicBool::new(false);
    let threads = req.limits.threads.clamp(1, 16).min(total.max(1));
    let worker = || {
        let mut slots = Vec::new();
        let mut opener = Opener::new(req.root);
        loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let index = next.fetch_add(1, Ordering::Relaxed);
            if index >= end {
                break;
            }
            let slot = scan_one(req, index, &bytes, &mut opener);
            if matches!(slot, Slot::NotReached(_)) {
                stop.store(true, Ordering::Relaxed);
            }
            slots.push((index, slot));
        }
        slots
    };
    let mut slots: Vec<Option<Slot>> = (0..total).map(|_| None).collect();
    let place = |slots: &mut Vec<Option<Slot>>, found: Vec<(usize, Slot)>| {
        for (index, slot) in found {
            slots[index - start] = Some(slot);
        }
    };
    if threads == 1 {
        place(&mut slots, worker());
    } else {
        let found: Vec<Vec<(usize, Slot)>> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..threads).map(|_| scope.spawn(worker)).collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap_or_default())
                .collect()
        });
        for batch in found {
            place(&mut slots, batch);
        }
    }

    let mut outcome = ScanOutcome {
        end,
        ..ScanOutcome::default()
    };
    for (offset, slot) in slots.into_iter().enumerate() {
        match slot {
            // A worker stopped claiming before this candidate.
            None => {
                outcome.end = start + offset;
                outcome.stopped = Some(stop_reason(req));
                break;
            }
            Some(Slot::NotReached(reason)) => {
                outcome.end = start + offset;
                outcome.stopped = Some(reason);
                break;
            }
            Some(Slot::TooLarge) => outcome.too_large += 1,
            Some(Slot::Nothing) => {}
            Some(Slot::Matched(hits)) => outcome.files.push(hits),
        }
    }
    outcome
}

/// The limit that is exhausted now (a worker that stopped claiming saw one).
fn stop_reason(req: &ScanRequest<'_>) -> StopReason {
    if req.cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        StopReason::Cancelled
    } else if Instant::now() >= req.limits.deadline {
        StopReason::Deadline
    } else {
        StopReason::Bytes
    }
}

fn scan_one(
    req: &ScanRequest<'_>,
    index: usize,
    bytes: &AtomicU64,
    opener: &mut Opener<'_>,
) -> Slot {
    if req.cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        return Slot::NotReached(StopReason::Cancelled);
    }
    let first = index == req.start;
    if !first && Instant::now() >= req.limits.deadline {
        return Slot::NotReached(StopReason::Deadline);
    }
    let candidate = &req.candidates[index];
    let Some(mut file) = opener.open(&candidate.path) else {
        return Slot::Nothing;
    };
    let size = match file.metadata() {
        Ok(metadata) if metadata.is_file() => metadata.len(),
        _ => return Slot::Nothing,
    };
    if size > req.limits.max_file_bytes {
        return Slot::TooLarge;
    }
    let before = bytes.fetch_add(size, Ordering::Relaxed);
    if !first && before.saturating_add(size) > req.limits.max_bytes {
        return Slot::NotReached(StopReason::Bytes);
    }
    let mut buf = Vec::with_capacity(size as usize + 1);
    if file.read_to_end(&mut buf).is_err() || looks_binary(&buf) {
        return Slot::Nothing;
    }
    match search_file(req, index, candidate, &buf) {
        Some(hits) => Slot::Matched(hits),
        None => Slot::Nothing,
    }
}

/// Match one file's contents. `None` when nothing matches.
fn search_file(
    req: &ScanRequest<'_>,
    index: usize,
    candidate: &Candidate,
    buf: &[u8],
) -> Option<FileHits> {
    let sent_ranges = if req.sent.tracks(&candidate.path) {
        req.sent.ranges(&candidate.path, &content_fingerprint(buf))
    } else {
        &[]
    };
    let per_file = req.max_per_file;
    let mut out = FileHits {
        index,
        count: 0,
        code_hits: 0,
        hits: Vec::new(),
        sent: Vec::new(),
        sent_total: 0,
    };
    // Unsent lines worth showing: code lines, and comments as filler.
    let mut code = Vec::new();
    let mut comments = Vec::new();
    let code_cap = per_file.saturating_mul(CODE_CANDIDATES_PER_SHOWN);
    for_each_matching_line(req.regex, buf, |found| {
        out.count += 1;
        let text = &buf[found.start..found.end];
        let comment = is_comment_line(candidate.language, text);
        if !comment {
            out.code_hits += 1;
        }
        if covers(sent_ranges, found.line as usize) {
            out.sent_total += 1;
            if out.sent.len() < MAX_SENT_LISTED {
                out.sent.push(found.line);
            }
            return true;
        }
        let (pool, cap) = if comment {
            (&mut comments, per_file)
        } else {
            (&mut code, code_cap)
        };
        if pool.len() < cap {
            pool.push(Hit {
                line: found.line,
                text: clip(text, found.column),
                comment,
            });
        }
        true
    });
    if out.count == 0 {
        return None;
    }
    out.hits = code;
    out.hits.extend(comments);
    out.hits.sort_by_key(|hit| hit.line);
    Some(out)
}

/// A hit's line, trimmed, and cut to [`MAX_HIT_CHARS`] around the match
/// when longer (`…` marks each cut side).
pub(in crate::mcp::tools) fn clip(line: &[u8], column: usize) -> String {
    let text = String::from_utf8_lossy(line);
    let leading = text.len() - text.trim_start().len();
    let trimmed = text.trim();
    let chars = trimmed.chars().count();
    if chars <= MAX_HIT_CHARS {
        return trimmed.to_string();
    }
    let column = String::from_utf8_lossy(&line[..column.min(line.len())])
        .len()
        .saturating_sub(leading);
    let match_char = trimmed
        .char_indices()
        .take_while(|(offset, _)| *offset < column)
        .count();
    // Keep a little context before the match, and the window full.
    let room = MAX_HIT_CHARS - 2;
    let first = match_char.saturating_sub(40).min(chars - room);
    let mut out = String::new();
    if first > 0 {
        out.push('…');
    }
    out.extend(trimmed.chars().skip(first).take(room));
    if first + room < chars {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::matcher::{PatternFlags, compile};
    use super::*;

    fn limits() -> ScanLimits {
        ScanLimits {
            deadline: Instant::now() + Duration::from_secs(30),
            max_bytes: u64::MAX,
            max_file_bytes: u64::MAX,
            threads: 4,
        }
    }

    fn fixture(files: &[(&str, &str)]) -> (tempfile::TempDir, Vec<Candidate>) {
        let dir = tempfile::tempdir().unwrap();
        let mut candidates = Vec::new();
        for (path, body) in files {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, body).unwrap();
            candidates.push(Candidate {
                path: (*path).to_string(),
                language: Language::Rust,
            });
        }
        (dir, candidates)
    }

    fn run(
        root: &Path,
        candidates: &[Candidate],
        pattern: &str,
        limits: ScanLimits,
        sent: &SentLines,
    ) -> ScanOutcome {
        let regex = compile(pattern, PatternFlags::default()).unwrap();
        scan(&ScanRequest {
            root,
            candidates,
            start: 0,
            end: None,
            regex: &regex,
            max_per_file: 2,
            sent,
            limits,
            cancel: None,
        })
    }

    #[test]
    fn keeps_candidate_order_and_counts_every_matching_line() {
        let files: Vec<(String, String)> = (0..40)
            .map(|i| {
                let body = if i % 3 == 0 {
                    "fn hit() {}\n// hit in a comment\nlet hit = 1;\nhit();\n".to_string()
                } else {
                    "fn miss() {}\n".to_string()
                };
                (format!("src/f{i:02}.rs"), body)
            })
            .collect();
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_str()))
            .collect();
        let (dir, candidates) = fixture(&borrowed);
        let outcome = run(
            dir.path(),
            &candidates,
            "hit",
            limits(),
            &SentLines::default(),
        );
        assert_eq!(outcome.stopped, None);
        assert_eq!(outcome.end, 40);
        let indexes: Vec<usize> = outcome.files.iter().map(|f| f.index).collect();
        assert_eq!(indexes, (0..40).step_by(3).collect::<Vec<_>>());
        let first = &outcome.files[0];
        assert_eq!((first.count, first.code_hits), (4, 3));
        let candidates: Vec<(u32, bool)> = first.hits.iter().map(|h| (h.line, h.comment)).collect();
        assert_eq!(
            candidates,
            vec![(1, false), (2, true), (3, false), (4, false)]
        );
    }

    #[test]
    fn candidates_are_bounded_per_kind() {
        let mut body = String::new();
        for line in 0..20 {
            body.push_str(if line % 2 == 0 {
                "// needle\n"
            } else {
                "needle();\n"
            });
        }
        let (dir, candidates) = fixture(&[("a.rs", body.as_str())]);
        let outcome = run(
            dir.path(),
            &candidates,
            "needle",
            limits(),
            &SentLines::default(),
        );
        let file = &outcome.files[0];
        assert_eq!(file.count, 20);
        let code = file.hits.iter().filter(|h| !h.comment).count();
        let comments = file.hits.iter().filter(|h| h.comment).count();
        // max_per_file is 2 in `run`.
        assert_eq!((code, comments), (2 * CODE_CANDIDATES_PER_SHOWN, 2));
        assert_eq!(
            file.hits.first(),
            Some(&Hit {
                line: 1,
                text: "// needle".into(),
                comment: true
            })
        );
    }

    /// The deadline is enforced per file: an expired one searches only the
    /// first file (so a cursor always advances) and says why it stopped.
    #[test]
    fn an_expired_deadline_searches_only_the_first_file() {
        let (dir, candidates) = fixture(&[
            ("a.rs", "needle\n"),
            ("b.rs", "needle\n"),
            ("c.rs", "needle\n"),
        ]);
        let expired = ScanLimits {
            deadline: Instant::now(),
            ..limits()
        };
        let outcome = run(
            dir.path(),
            &candidates,
            "needle",
            expired,
            &SentLines::default(),
        );
        assert_eq!(outcome.stopped, Some(StopReason::Deadline));
        assert_eq!(outcome.end, 1);
        assert_eq!(outcome.files.len(), 1);
        assert_eq!(outcome.files[0].index, 0);
    }

    #[test]
    fn the_byte_budget_stops_at_a_file_boundary() {
        let files: Vec<(String, String)> = (0..20)
            .map(|i| (format!("f{i:02}.rs"), "needle\n".repeat(10)))
            .collect();
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_str()))
            .collect();
        let (dir, candidates) = fixture(&borrowed);
        let tight = ScanLimits {
            max_bytes: 5 * 70,
            threads: 1,
            ..limits()
        };
        let outcome = run(
            dir.path(),
            &candidates,
            "needle",
            tight,
            &SentLines::default(),
        );
        assert_eq!(outcome.stopped, Some(StopReason::Bytes));
        assert_eq!(outcome.end, 5);
        assert_eq!(outcome.files.len(), 5);
        assert!(outcome.files.iter().all(|f| f.count == 10));
    }

    #[test]
    fn oversized_and_binary_files_are_skipped() {
        let (dir, candidates) = fixture(&[
            ("big.rs", &"needle\n".repeat(100)),
            ("bin.rs", "needle\0\n"),
            ("ok.rs", "needle\n"),
        ]);
        let small = ScanLimits {
            max_file_bytes: 100,
            ..limits()
        };
        let outcome = run(
            dir.path(),
            &candidates,
            "needle",
            small,
            &SentLines::default(),
        );
        assert_eq!(outcome.too_large, 1);
        assert_eq!(outcome.files.len(), 1);
        assert_eq!(outcome.files[0].index, 2);
    }

    #[test]
    fn lines_the_session_holds_are_listed_not_resent() {
        use crate::mcp::explore_session::{GrepSent, GrepSentFile, content_fingerprint};
        let body = "needle 1\nneedle 2\nneedle 3\n";
        let (dir, candidates) = fixture(&[("a.rs", body)]);
        let grep = GrepSent {
            files: vec![GrepSentFile {
                path: "a.rs".into(),
                fingerprint: content_fingerprint(body.as_bytes()),
                lines: vec![1, 2],
            }]
            .into(),
        };
        let sent = SentLines::from_views(None, Some(&grep));
        let outcome = run(dir.path(), &candidates, "needle", limits(), &sent);
        let file = &outcome.files[0];
        assert_eq!(file.count, 3);
        assert_eq!(file.sent, vec![1, 2]);
        assert_eq!(
            file.hits.iter().map(|h| h.line).collect::<Vec<_>>(),
            vec![3]
        );

        // After an edit the old record no longer applies.
        std::fs::write(dir.path().join("a.rs"), format!("{body}needle 4\n")).unwrap();
        let outcome = run(dir.path(), &candidates, "needle", limits(), &sent);
        assert!(outcome.files[0].sent.is_empty());
    }

    #[test]
    fn clip_trims_and_windows_long_lines_around_the_match() {
        assert_eq!(clip(b"    let x = 1;   ", 8), "let x = 1;");
        let long = format!("{}needle{}", "a".repeat(300), "b".repeat(300));
        let clipped = clip(long.as_bytes(), 300);
        assert_eq!(clipped.chars().count(), MAX_HIT_CHARS);
        assert!(clipped.starts_with('…') && clipped.ends_with('…'));
        assert!(clipped.contains("needle"));
        let head = format!("needle{}", "b".repeat(300));
        let clipped = clip(head.as_bytes(), 0);
        assert!(clipped.starts_with("needle") && clipped.ends_with('…'));
        assert_eq!(clipped.chars().count(), MAX_HIT_CHARS - 1);
    }
}

//! What `codegraph_grep` already sent in this session.
//!
//! Grep hits are single, possibly shortened lines, so they are kept apart
//! from the source ranges explore and node record: those tools treat any
//! recorded range as the file being "already held", which a one-line
//! excerpt must never cause. Grep reads both — the ranges (lines the session
//! holds verbatim) and its own sent lines — and leaves hits on either out.

use std::collections::VecDeque;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::file_fingerprint;

/// Argument the service injects into `codegraph_grep` calls: a [`GrepSent`].
pub(crate) const GREP_SESSION_ARG: &str = "_cgGrepSession";
/// Files remembered per project, most recent last.
const MAX_FILES: usize = 128;
/// Lines remembered per file.
const MAX_LINES: usize = 256;

/// Lines of one file sent as grep hits, valid while the file's fingerprint
/// is unchanged.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GrepSentFile {
    pub path: String,
    pub fingerprint: String,
    /// Sorted, deduplicated 1-based line numbers.
    pub lines: Vec<usize>,
}

/// One project's grep ledger (also the view injected into a call).
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GrepSent {
    pub files: VecDeque<GrepSentFile>,
}

impl GrepSent {
    /// Lines of `path` already sent while it had `fingerprint`.
    #[cfg(test)]
    pub(crate) fn lines(&self, path: &str, fingerprint: &str) -> &[usize] {
        self.files
            .iter()
            .find(|file| file.path == path && file.fingerprint == fingerprint)
            .map_or(&[], |file| file.lines.as_slice())
    }

    /// Record the hits a delivered `grep` payload carried: its matching
    /// `N: text` lines. Context lines (`N- text`) and withheld values (a
    /// bare `N`) are not hits the session was sent.
    pub(crate) fn record(&mut self, project_root: &Path, payload: &Value) {
        for file in payload["files"].as_array().into_iter().flatten() {
            let Some(path) = file["file"].as_str() else {
                continue;
            };
            let lines: Vec<usize> = file["hits"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|group| group["lines"].as_array().into_iter().flatten())
                .filter_map(Value::as_str)
                .filter_map(sent_hit_line)
                .collect();
            if lines.is_empty() {
                continue;
            }
            let Some(fingerprint) = file_fingerprint(project_root, path) else {
                continue;
            };
            self.add(path, &fingerprint, lines);
        }
    }

    fn add(&mut self, path: &str, fingerprint: &str, lines: Vec<usize>) {
        let mut entry = match self.files.iter().position(|file| file.path == path) {
            Some(index) => self.files.remove(index).unwrap_or_default(),
            None => GrepSentFile::default(),
        };
        if entry.fingerprint != fingerprint {
            entry = GrepSentFile {
                path: path.to_string(),
                fingerprint: fingerprint.to_string(),
                lines: Vec::new(),
            };
        }
        entry.lines.extend(lines);
        entry.lines.sort_unstable();
        entry.lines.dedup();
        if entry.lines.len() > MAX_LINES {
            let excess = entry.lines.len() - MAX_LINES;
            entry.lines.drain(..excess);
        }
        self.files.push_back(entry);
        while self.files.len() > MAX_FILES {
            self.files.pop_front();
        }
    }
}

/// The line number of a matching `N: text` hit line.
fn sent_hit_line(line: &str) -> Option<usize> {
    let (number, _) = line.split_once(':')?;
    number.parse().ok()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn records_sent_hits_per_fingerprint_and_skips_withheld_ones() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.rs"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(root.path().join("c.toml"), "k = 1\n").unwrap();
        let mut sent = GrepSent::default();
        sent.record(
            root.path(),
            &json!({
                "kind": "grep",
                "files": [
                    { "file": "a.rs", "count": 3, "hits": [
                        { "symbol": "f", "lines": ["3: three", "4- context"] },
                        { "lines": ["1: one"] }
                    ]},
                    { "file": "c.toml", "count": 1, "hits": [{ "lines": ["1"] }] },
                    { "file": "missing.rs", "count": 1, "hits": [{ "lines": ["1: x"] }] }
                ]
            }),
        );
        let fingerprint = file_fingerprint(root.path(), "a.rs").unwrap();
        assert_eq!(sent.lines("a.rs", &fingerprint), &[1, 3]);
        assert_eq!(
            sent.files.len(),
            1,
            "withheld and unreadable files are not recorded"
        );

        // A second call adds to the same file; an edit invalidates it.
        sent.add("a.rs", &fingerprint, vec![2]);
        assert_eq!(sent.lines("a.rs", &fingerprint), &[1, 2, 3]);
        std::fs::write(root.path().join("a.rs"), "one\nTWO\nthree\n").unwrap();
        let edited = file_fingerprint(root.path(), "a.rs").unwrap();
        assert!(sent.lines("a.rs", &edited).is_empty());
        sent.add("a.rs", &edited, vec![2]);
        assert_eq!(sent.lines("a.rs", &edited), &[2]);
        assert!(sent.lines("a.rs", &fingerprint).is_empty());
    }

    #[test]
    fn stays_bounded() {
        let mut sent = GrepSent::default();
        for index in 0..MAX_FILES + 10 {
            sent.add(&format!("f{index}.rs"), "fp", (1..=MAX_LINES + 5).collect());
        }
        assert_eq!(sent.files.len(), MAX_FILES);
        assert!(sent.files.iter().all(|file| file.lines.len() == MAX_LINES));
        assert_eq!(sent.files.front().unwrap().path, "f10.rs");
    }
}

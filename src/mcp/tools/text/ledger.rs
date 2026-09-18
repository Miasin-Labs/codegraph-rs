//! Lines this session already received, as the grep scanner asks for them.
//!
//! Two sources feed it: the source ranges `explore`/`node` sent (verbatim
//! lines) and the lines earlier `grep` calls sent as hits. Both are keyed by
//! the file's content fingerprint, so an edited file is never deduplicated
//! against what it used to say.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::mcp::explore_session::{GREP_SESSION_ARG, GrepSent, ProjectState, SESSION_ARG};

/// Sent line ranges (inclusive) of one file version.
type Versions = Vec<(String, Vec<(usize, usize)>)>;

#[derive(Debug, Default)]
pub(in crate::mcp::tools) struct SentLines {
    by_path: HashMap<String, Versions>,
}

impl SentLines {
    /// Read the ledger views the service injected into this call's
    /// arguments (none outside an MCP session).
    pub fn from_args(args: &Map<String, Value>) -> Self {
        let prior = args
            .get(SESSION_ARG)
            .and_then(|value| serde_json::from_value::<ProjectState>(value.clone()).ok());
        let grep = args
            .get(GREP_SESSION_ARG)
            .and_then(|value| serde_json::from_value::<GrepSent>(value.clone()).ok());
        Self::from_views(prior.as_ref(), grep.as_ref())
    }

    pub fn from_views(prior: Option<&ProjectState>, grep: Option<&GrepSent>) -> Self {
        let mut sent = Self::default();
        for file in prior
            .iter()
            .flat_map(|prior| &prior.calls)
            .flat_map(|call| &call.files)
        {
            let Some(fingerprint) = &file.fingerprint else {
                continue;
            };
            let ranges = file.ranges.iter().map(|range| (range.start, range.end));
            sent.add(&file.path, fingerprint, ranges);
        }
        for file in grep.iter().flat_map(|grep| &grep.files) {
            let lines = file.lines.iter().map(|&line| (line, line));
            sent.add(&file.path, &file.fingerprint, lines);
        }
        sent
    }

    fn add(&mut self, path: &str, fingerprint: &str, ranges: impl Iterator<Item = (usize, usize)>) {
        let versions = self.by_path.entry(path.to_string()).or_default();
        let index = match versions.iter().position(|(known, _)| known == fingerprint) {
            Some(index) => index,
            None => {
                versions.push((fingerprint.to_string(), Vec::new()));
                versions.len() - 1
            }
        };
        versions[index].1.extend(ranges);
    }

    /// Whether any version of `path` has sent lines (the scanner only
    /// fingerprints those files).
    pub fn tracks(&self, path: &str) -> bool {
        self.by_path.contains_key(path)
    }

    /// Sent ranges of `path` while it had `fingerprint`.
    pub fn ranges(&self, path: &str, fingerprint: &str) -> &[(usize, usize)] {
        self.by_path
            .get(path)
            .and_then(|versions| versions.iter().find(|(known, _)| known == fingerprint))
            .map_or(&[], |(_, ranges)| ranges.as_slice())
    }
}

/// Whether `line` falls in one of `ranges`.
pub(in crate::mcp::tools) fn covers(ranges: &[(usize, usize)], line: usize) -> bool {
    ranges
        .iter()
        .any(|&(start, end)| start <= line && line <= end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::explore_session::{CallRecord, FileEmission, GrepSentFile, LineRange};

    #[test]
    fn merges_source_ranges_and_grep_lines_per_fingerprint() {
        let prior = ProjectState {
            project_root: "/p".into(),
            call_count: 1,
            response_bytes: 0,
            calls: vec![CallRecord {
                index: 1,
                response_bytes: 0,
                files: vec![FileEmission {
                    path: "a.rs".into(),
                    ranges: vec![LineRange { start: 10, end: 20 }],
                    bytes: 0,
                    fingerprint: Some("v1".into()),
                }],
            }],
        };
        let grep = GrepSent {
            files: vec![GrepSentFile {
                path: "a.rs".into(),
                fingerprint: "v1".into(),
                lines: vec![42],
            }]
            .into(),
        };
        let sent = SentLines::from_views(Some(&prior), Some(&grep));
        assert!(sent.tracks("a.rs"));
        assert!(!sent.tracks("b.rs"));
        let ranges = sent.ranges("a.rs", "v1");
        assert!(covers(ranges, 15) && covers(ranges, 42));
        assert!(!covers(ranges, 21));
        assert!(sent.ranges("a.rs", "v2").is_empty());
    }
}

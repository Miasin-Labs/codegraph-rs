//! Persistent content index for `graph_grep`.
//!
//! Two caches, both keyed by file path, both validated against the file's
//! modification time so a stale entry is silently refreshed:
//!
//! 1. **Line cache** — the file's lines, read once and reused across grep
//!    calls. Without it, every `graph_grep` re-read every indexed file from
//!    disk (the limitation flagged in the search→sed fix).
//!
//! 2. **Symbol-span index** — a per-file, start-line-sorted list of
//!    `(start, end, name)` spans for Function/Struct nodes. Enclosing-symbol
//!    lookup becomes a binary search instead of an O(N) scan over *every*
//!    graph node for *every* match (the old `enclosing_symbol` was O(M×N)).
//!
//! The line cache uses `DashMap` for interior mutability through `&self`,
//! matching the `QueryCache` pattern in [`crate::incremental`], so it slots
//! into the `Arc<GraphSession>` read path without a `&mut` borrow. The span
//! index is a whole-graph `file → spans` map behind a `Mutex`, rebuilt in a
//! single O(N) pass whenever the graph revision advances (rebuilding per
//! file cost O(F×N) over a cold cache).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use dashmap::DashMap;

use crate::graph::CodeGraph;
use crate::nodes::NodeKind;

/// One file's cached lines plus the mtime they were read at.
struct LineEntry {
    mtime: Option<SystemTime>,
    lines: Arc<Vec<String>>,
}

/// A symbol span used for enclosing-symbol resolution. Sorted by `start`.
#[derive(Clone)]
struct SymbolSpan {
    start: u32,
    end: u32,
    name: String,
}

/// Every file's symbol spans plus the graph revision they were built at.
/// Built for ALL files in one O(N) pass over the graph, so a cold cache
/// over F files costs O(N) instead of O(F×N).
struct SpanCache {
    revision: u64,
    by_file: HashMap<PathBuf, Arc<Vec<SymbolSpan>>>,
    /// Shared empty list returned for files with no Function/Struct spans.
    empty: Arc<Vec<SymbolSpan>>,
}

/// Caches file content + symbol spans for fast repeated content search.
#[derive(Default)]
pub struct ContentIndex {
    lines: DashMap<PathBuf, LineEntry>,
    spans: Mutex<Option<SpanCache>>,
}

impl ContentIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Cached file lines, refreshed when the on-disk mtime changes.
    /// Returns `None` if the file can't be read.
    pub fn lines(&self, path: &Path) -> Option<Arc<Vec<String>>> {
        let disk_mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();

        if let Some(entry) = self.lines.get(path)
            && entry.mtime == disk_mtime
        {
            return Some(Arc::clone(&entry.lines));
        }

        // Miss or stale — read and cache.
        let content = std::fs::read_to_string(path).ok()?;
        let lines: Arc<Vec<String>> = Arc::new(content.lines().map(str::to_owned).collect());
        self.lines.insert(
            path.to_path_buf(),
            LineEntry {
                mtime: disk_mtime,
                lines: Arc::clone(&lines),
            },
        );
        Some(lines)
    }

    /// Cached lines `start..=end` (1-indexed, inclusive) of `file`, returned
    /// as owned strings. Used by the body-rendering read paths (`graph_node`,
    /// `graph_search include_code`, `graph_explore`) so they share the same
    /// mtime-validated cache as `graph_grep` instead of re-reading from disk.
    /// Returns `None` if the file is unreadable or the range is degenerate.
    pub fn span_lines(&self, file: &Path, start: u32, end: u32) -> Option<Vec<String>> {
        let lines = self.lines(file)?;
        let lo = start.saturating_sub(1) as usize;
        let hi = (end as usize).min(lines.len());
        if lo >= hi {
            return None;
        }
        Some(lines[lo..hi].to_vec())
    }

    /// Innermost enclosing Function/Struct symbol at `line` in `file`, using
    /// a cached, start-sorted span list (binary search). `graph` is consulted
    /// only on a cache miss or when the graph revision advanced.
    pub fn enclosing_symbol(&self, graph: &CodeGraph, file: &Path, line: u32) -> Option<String> {
        let spans = self.spans_for(graph, file);
        // `spans` is sorted by `start`. Among all spans whose
        // [start, end] contains `line`, the innermost is the one with the
        // largest `start` (equivalently the smallest span, since they nest).
        // Walk the prefix with `start <= line` from the back.
        let upper = spans.partition_point(|s| s.start <= line);
        spans[..upper]
            .iter()
            .rev()
            .filter(|s| s.end >= line)
            .min_by_key(|s| s.end.saturating_sub(s.start))
            .map(|s| s.name.clone())
    }

    /// Get (or build) the start-sorted span list for `file`.
    ///
    /// The whole `file → spans` map is rebuilt in a single O(N) pass over
    /// the graph when the revision advances; per-file lookups are then
    /// hash-map hits. (Previously each cold file ran its own O(N)
    /// full-graph scan, so a cold cache over F files cost O(F×N).)
    fn spans_for(&self, graph: &CodeGraph, file: &Path) -> Arc<Vec<SymbolSpan>> {
        let rev = graph.current_revision();
        let mut guard = self.spans.lock().unwrap_or_else(|e| e.into_inner());

        if guard.as_ref().is_none_or(|cache| cache.revision != rev) {
            let mut grouped: HashMap<PathBuf, Vec<SymbolSpan>> = HashMap::new();
            for id in graph.all_node_ids() {
                let Some(n) = graph.get_node(id) else {
                    continue;
                };
                if !matches!(n.kind, NodeKind::Function | NodeKind::Struct) {
                    continue;
                }
                grouped
                    .entry(n.file_path.clone())
                    .or_default()
                    .push(SymbolSpan {
                        start: n.span.start_line,
                        end: n.span.end_line,
                        name: n.name.clone(),
                    });
            }
            let by_file = grouped
                .into_iter()
                .map(|(path, mut spans)| {
                    spans.sort_by_key(|s| s.start);
                    (path, Arc::new(spans))
                })
                .collect();
            *guard = Some(SpanCache {
                revision: rev,
                by_file,
                empty: Arc::new(Vec::new()),
            });
        }

        let cache = guard.as_ref().expect("span cache rebuilt above");
        cache
            .by_file
            .get(file)
            .map_or_else(|| Arc::clone(&cache.empty), Arc::clone)
    }

    /// Drop cached state for one file (called when a file changes). The span
    /// cache is revision-gated and whole-graph — absence of a file in it
    /// means "no spans" — so it is cleared outright to force a one-pass
    /// rebuild rather than removing a single entry, which would be misread
    /// as an empty span list.
    pub fn invalidate(&self, file: &Path) {
        self.lines.remove(file);
        *self.spans.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Number of files with cached lines (diagnostics / tests).
    pub fn cached_file_count(&self) -> usize {
        self.lines.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_cache_hits_without_rereading() {
        let dir = std::env::temp_dir().join(format!("codegraph-ci-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.txt");
        std::fs::write(&f, "one\ntwo\nthree\n").unwrap();

        let idx = ContentIndex::new();
        let first = idx.lines(&f).unwrap();
        assert_eq!(first.len(), 3);
        assert_eq!(idx.cached_file_count(), 1);

        // Second call returns the same Arc (cache hit).
        let second = idx.lines(&f).unwrap();
        assert!(
            Arc::ptr_eq(&first, &second),
            "cache should return the same Arc"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lines_cache_refreshes_on_mtime_change() {
        let dir = std::env::temp_dir().join(format!("codegraph-ci-mtime-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("b.txt");
        std::fs::write(&f, "v1\n").unwrap();

        let idx = ContentIndex::new();
        let first = idx.lines(&f).unwrap();
        assert_eq!(first.as_slice(), &["v1".to_string()]);

        // Rewrite with a guaranteed-later mtime (sleep covers coarse FS
        // timestamp resolution).
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&f, "v1\nv2\n").unwrap();

        let second = idx.lines(&f).unwrap();
        assert_eq!(
            second.len(),
            2,
            "stale entry should refresh after mtime change"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalidate_drops_entry() {
        let dir = std::env::temp_dir().join(format!("codegraph-ci-inval-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("c.txt");
        std::fs::write(&f, "x\n").unwrap();

        let idx = ContentIndex::new();
        idx.lines(&f).unwrap();
        assert_eq!(idx.cached_file_count(), 1);
        idx.invalidate(&f);
        assert_eq!(idx.cached_file_count(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

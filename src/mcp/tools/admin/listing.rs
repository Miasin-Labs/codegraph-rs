//! The structured `codegraph_files` listing: files grouped by directory,
//! cut at a depth below the requested `path` (deeper directories collapse
//! into file counts), and paged to the output budget.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::super::format::{json_len, locale_cmp};
use super::super::output::FileDirOutput;
use crate::utils::sha256_hex;

/// One indexed file as the listing sees it.
pub(super) struct IndexedFile<'a> {
    pub path: &'a str,
    pub symbols: u32,
}

/// One row of a listing: a file inside `dir`, or a subdirectory of `dir`
/// deeper than the depth limit, collapsed to its file count.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    Collapsed {
        dir: String,
        name: String,
        files: usize,
    },
    File {
        dir: String,
        name: String,
        symbols: u32,
    },
}

impl Entry {
    fn dir(&self) -> &str {
        match self {
            Self::Collapsed { dir, .. } | Self::File { dir, .. } => dir,
        }
    }

    fn name(&self) -> &str {
        match self {
            Self::Collapsed { name, .. } | Self::File { name, .. } => name,
        }
    }

    /// Serialized cost inside its directory's map: `"name":N,`.
    fn cost(&self) -> usize {
        let count = match self {
            Self::Collapsed { files, .. } => files.to_string().len(),
            Self::File { symbols, .. } => symbols.to_string().len(),
        };
        json_len(self.name()) + 1 + count + 1
    }
}

/// Serialized cost of opening a directory object: `{"path":"…"},` plus the
/// `"files":{}` and `"dirs":{}` wrappers it may need.
fn dir_cost(dir: &str) -> usize {
    json_len(dir) + r#"{"path":,"files":{},"dirs":{}},"#.len()
}

/// A complete, ordered listing at one depth.
pub(super) struct Listing {
    entries: Vec<Entry>,
}

/// The directory holding `path`, `.` for the project root.
fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or(".", |(dir, _)| dir)
}

/// `path` relative to `base` (both project-relative, `base` possibly empty).
fn relative<'a>(path: &'a str, base: &str) -> &'a str {
    if base.is_empty() {
        return path;
    }
    if path == base {
        return path.rsplit_once('/').map_or(path, |(_, name)| name);
    }
    path.strip_prefix(base)
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(path)
}

impl Listing {
    /// List `files` (all under `base`), cutting at `max_depth` path
    /// components below `base`.
    pub fn build(files: &[IndexedFile<'_>], base: &str, max_depth: Option<usize>) -> Self {
        let mut listed = Vec::new();
        let mut collapsed: BTreeMap<(String, String), usize> = BTreeMap::new();
        for file in files {
            let rel = relative(file.path, base);
            let parts: Vec<&str> = rel.split('/').filter(|part| !part.is_empty()).collect();
            match max_depth {
                Some(depth) if parts.len() > depth => {
                    let frontier_rel = parts[..depth].join("/");
                    let frontier = if base.is_empty() {
                        frontier_rel
                    } else {
                        format!("{base}/{frontier_rel}")
                    };
                    let key = (parent(&frontier).to_string(), parts[depth - 1].to_string());
                    *collapsed.entry(key).or_default() += 1;
                }
                _ => {
                    let (dir, name) = file
                        .path
                        .rsplit_once('/')
                        .map_or((".", file.path), |(dir, name)| (dir, name));
                    listed.push(Entry::File {
                        dir: dir.to_string(),
                        name: name.to_string(),
                        symbols: file.symbols,
                    });
                }
            }
        }
        let mut entries: Vec<Entry> = collapsed
            .into_iter()
            .map(|((dir, name), files)| Entry::Collapsed { dir, name, files })
            .chain(listed)
            .collect();
        // Directory by directory; within one, subdirectories before files.
        entries.sort_by(|a, b| {
            locale_cmp(a.dir(), b.dir())
                .then_with(|| {
                    matches!(b, Entry::Collapsed { .. }).cmp(&matches!(a, Entry::Collapsed { .. }))
                })
                .then_with(|| locale_cmp(a.name(), b.name()))
        });
        Self { entries }
    }

    /// Deepest path below `base` among `files`, in components.
    pub fn deepest(files: &[IndexedFile<'_>], base: &str) -> usize {
        files
            .iter()
            .map(|file| {
                relative(file.path, base)
                    .split('/')
                    .filter(|part| !part.is_empty())
                    .count()
            })
            .max()
            .unwrap_or(1)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Serialized size of the whole listing.
    pub fn cost(&self) -> usize {
        self.cost_of(0, self.entries.len())
    }

    fn cost_of(&self, start: usize, end: usize) -> usize {
        let mut total = 0;
        let mut current: Option<&str> = None;
        for entry in &self.entries[start..end] {
            if current != Some(entry.dir()) {
                current = Some(entry.dir());
                total += dir_cost(entry.dir());
            }
            total += entry.cost();
        }
        total
    }

    /// Entries from `offset` that fit in `room` characters; returns the
    /// index one past the last one taken (at least one entry when any
    /// remain, so paging always advances).
    pub fn page_end(&self, offset: usize, room: usize) -> usize {
        let mut used = 0;
        let mut current: Option<&str> = None;
        let mut end = offset;
        for entry in self.entries.iter().skip(offset) {
            let mut cost = entry.cost();
            if current != Some(entry.dir()) {
                cost += dir_cost(entry.dir());
            }
            if used + cost > room && end > offset {
                break;
            }
            used += cost;
            current = Some(entry.dir());
            end += 1;
        }
        end
    }

    /// Entries `start..end` grouped into directory objects.
    pub fn dirs(&self, start: usize, end: usize) -> Vec<FileDirOutput> {
        let mut out: Vec<FileDirOutput> = Vec::new();
        for entry in &self.entries[start..end.min(self.entries.len())] {
            if out.last().is_none_or(|dir| dir.path != entry.dir()) {
                out.push(FileDirOutput {
                    path: entry.dir().to_string(),
                    files: Map::new(),
                    dirs: Map::new(),
                });
            }
            let Some(dir) = out.last_mut() else {
                continue;
            };
            match entry {
                Entry::Collapsed { name, files, .. } => {
                    dir.dirs.insert(name.clone(), Value::from(*files));
                }
                Entry::File { name, symbols, .. } => {
                    dir.files.insert(name.clone(), Value::from(*symbols));
                }
            }
        }
        out
    }
}

/// Where the next page of a listing starts. The cursor names the depth the
/// listing was cut at (so later pages agree with the first) and a digest of
/// the `path`/`pattern` it lists (so it cannot continue a different one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Cursor {
    pub depth: Option<usize>,
    pub auto_depth: bool,
    pub offset: usize,
}

pub(super) fn listing_key(path: &str, pattern: &str) -> String {
    sha256_hex(format!("{path}\u{0}{pattern}").as_bytes())[..8].to_string()
}

impl Cursor {
    pub fn encode(self, key: &str) -> String {
        format!(
            "{}{}:{}:{key}",
            self.depth.unwrap_or(0),
            if self.auto_depth { "a" } else { "" },
            self.offset
        )
    }

    /// Parse a cursor minted for the listing `key`.
    pub fn decode(raw: &str, key: &str) -> Option<Self> {
        let mut parts = raw.trim().split(':');
        let depth = parts.next()?;
        let offset = parts.next()?.parse().ok()?;
        if parts.next()? != key || parts.next().is_some() {
            return None;
        }
        let (depth, auto_depth) = match depth.strip_suffix('a') {
            Some(depth) => (depth, true),
            None => (depth, false),
        };
        let depth: usize = depth.parse().ok()?;
        Some(Self {
            depth: (depth > 0).then_some(depth),
            auto_depth,
            offset,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(paths: &[&str]) -> Vec<IndexedFile<'static>> {
        paths
            .iter()
            .map(|path| IndexedFile {
                path: Box::leak(path.to_string().into_boxed_str()),
                symbols: 1,
            })
            .collect()
    }

    fn listing_json(listing: &Listing) -> Value {
        serde_json::to_value(listing.dirs(0, listing.len())).unwrap()
    }

    #[test]
    fn collapses_directories_deeper_than_the_limit() {
        let files = files(&["Cargo.toml", "src/lib.rs", "src/mcp/a.rs", "src/mcp/b/c.rs"]);
        let listing = Listing::build(&files, "", Some(1));
        assert_eq!(
            listing_json(&listing),
            serde_json::json!([{ "path": ".", "files": { "Cargo.toml": 1 }, "dirs": { "src": 3 } }])
        );
        let listing = Listing::build(&files, "", Some(2));
        assert_eq!(
            listing_json(&listing),
            serde_json::json!([
                { "path": ".", "files": { "Cargo.toml": 1 } },
                { "path": "src", "files": { "lib.rs": 1 }, "dirs": { "mcp": 2 } }
            ])
        );
    }

    #[test]
    fn depth_counts_from_the_path_filter() {
        let files = files(&["src/lib.rs", "src/mcp/a.rs", "src/mcp/b/c.rs"]);
        let listing = Listing::build(&files, "src", Some(1));
        assert_eq!(
            listing_json(&listing),
            serde_json::json!([{ "path": "src", "files": { "lib.rs": 1 }, "dirs": { "mcp": 2 } }])
        );
        assert_eq!(Listing::deepest(&files, "src"), 3);
    }

    #[test]
    fn pages_cover_every_entry_once_and_fit_their_room() {
        let paths: Vec<String> = (0..200).map(|i| format!("d{}/f{i}.rs", i % 7)).collect();
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        let files = files(&refs);
        let listing = Listing::build(&files, "", None);
        let mut offset = 0;
        let mut seen = 0;
        while offset < listing.len() {
            let end = listing.page_end(offset, 600);
            assert!(end > offset);
            let page = serde_json::to_string(&listing.dirs(offset, end)).unwrap();
            assert!(page.len() <= 600, "page of {} chars", page.len());
            seen += end - offset;
            offset = end;
        }
        assert_eq!(seen, 200);
        assert!(
            listing.cost()
                >= serde_json::to_string(&listing_json(&listing))
                    .unwrap()
                    .len()
        );
    }

    #[test]
    fn cursor_round_trips_and_rejects_other_listings() {
        let key = listing_key("src", "*.rs");
        let cursor = Cursor {
            depth: Some(2),
            auto_depth: true,
            offset: 40,
        };
        assert_eq!(Cursor::decode(&cursor.encode(&key), &key), Some(cursor));
        assert_eq!(
            Cursor::decode(&cursor.encode(&key), &listing_key("", "")),
            None
        );
        assert_eq!(Cursor::decode("garbage", &key), None);
        let unlimited = Cursor {
            depth: None,
            auto_depth: false,
            offset: 3,
        };
        assert_eq!(
            Cursor::decode(&unlimited.encode(&key), &key),
            Some(unlimited)
        );
    }
}

//! Which indexed files a search covers: a `path` prefix and a `glob`.
//!
//! Only files the index holds are candidates, so the indexer's ignore rules
//! (gitignore, vendored and build output, unsupported types) apply as they
//! did when the project was indexed.

use std::path::Path;

use globset::{GlobBuilder, GlobMatcher};

use super::scan::Candidate;
use crate::types::Language;

/// A `glob` argument: ripgrep's `-g` semantics — without a `/` it matches
/// the file name at any depth, with one the whole project-relative path; a
/// leading `!` excludes.
#[derive(Debug)]
pub(in crate::mcp::tools) struct GlobFilter {
    matcher: GlobMatcher,
    whole_path: bool,
    negated: bool,
}

impl GlobFilter {
    pub fn parse(glob: &str) -> std::result::Result<Self, String> {
        let (negated, glob) = match glob.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, glob),
        };
        let glob = glob.trim_start_matches("./");
        let matcher = GlobBuilder::new(glob)
            .literal_separator(true)
            .build()
            .map_err(|error| error.to_string())?
            .compile_matcher();
        Ok(Self {
            matcher,
            whole_path: glob.contains('/'),
            negated,
        })
    }

    pub fn matches(&self, path: &str) -> bool {
        let subject = if self.whole_path {
            path
        } else {
            path.rsplit('/').next().unwrap_or(path)
        };
        self.matcher.is_match(subject) != self.negated
    }
}

/// The `path` and `glob` arguments, resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::mcp::tools) struct Scope {
    /// Project-relative directory or file; empty for the whole project.
    pub prefix: String,
    pub glob: Option<String>,
}

impl Scope {
    /// Normalize `path` (absolute paths inside the project are accepted).
    /// A glob given as `path` (`src/**/*.rs`) is taken as the glob it is,
    /// with its literal leading directories as the path.
    pub fn resolve(root: &Path, path: Option<&str>, glob: Option<&str>) -> Self {
        let prefix = path
            .map(|path| normalize_path(root, path))
            .unwrap_or_default();
        let is_glob = |text: &str| text.contains(['*', '?', '[', '{']);
        if glob.is_none() && is_glob(&prefix) {
            let literal: Vec<&str> = prefix
                .split('/')
                .take_while(|segment| !is_glob(segment))
                .collect();
            return Self {
                prefix: literal.join("/"),
                glob: Some(prefix),
            };
        }
        Self {
            prefix,
            glob: glob.map(str::to_string),
        }
    }
}

/// Normalize a `path` argument to a project-relative prefix (empty for the
/// whole project). Absolute paths inside the project are accepted.
fn normalize_path(root: &Path, raw: &str) -> String {
    let raw = raw.trim().replace('\\', "/");
    let root_text = root.to_string_lossy().replace('\\', "/");
    let relative = raw
        .strip_prefix(root_text.trim_end_matches('/'))
        .filter(|rest| rest.is_empty() || rest.starts_with('/'))
        .unwrap_or(&raw);
    let mut normalized = relative.trim_start_matches('/');
    while let Some(rest) = normalized.strip_prefix("./") {
        normalized = rest;
    }
    let normalized = normalized.trim_end_matches('/');
    if normalized == "." {
        String::new()
    } else {
        normalized.to_string()
    }
}

/// The indexed `(path, language)` rows that pass `glob`, in path order.
pub(in crate::mcp::tools) fn candidates(
    files: Vec<(String, Language)>,
    glob: Option<&GlobFilter>,
) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = files
        .into_iter()
        .filter(|(path, _)| glob.is_none_or(|glob| glob.matches(path)))
        .map(|(path, language)| Candidate { path, language })
        .collect();
    // The index returns path order already; the cursor depends on it.
    if !out.is_sorted_by(|a, b| a.path <= b.path) {
        out.sort_by(|a, b| a.path.cmp(&b.path));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_names_without_a_slash_and_paths_with_one() {
        let rs = GlobFilter::parse("*.rs").unwrap();
        assert!(rs.matches("src/deep/lib.rs"));
        assert!(!rs.matches("src/lib.rsx"));
        let braces = GlobFilter::parse("*.{c,h}").unwrap();
        assert!(braces.matches("net/core/skbuff.c") && braces.matches("include/x.h"));
        let rooted = GlobFilter::parse("src/*.rs").unwrap();
        assert!(rooted.matches("src/lib.rs"));
        assert!(!rooted.matches("src/deep/lib.rs"));
        let deep = GlobFilter::parse("src/**/*.rs").unwrap();
        assert!(deep.matches("src/deep/lib.rs"));
        let not_tests = GlobFilter::parse("!*_test.go").unwrap();
        assert!(not_tests.matches("pkg/a.go") && !not_tests.matches("pkg/a_test.go"));
        assert!(GlobFilter::parse("a[").is_err());
    }

    #[test]
    fn a_glob_given_as_path_becomes_the_glob() {
        let root = Path::new("/work/proj");
        assert_eq!(
            Scope::resolve(root, Some("src/mcp/*.rs"), None),
            Scope {
                prefix: "src/mcp".into(),
                glob: Some("src/mcp/*.rs".into())
            }
        );
        assert_eq!(
            Scope::resolve(root, Some("**/*.ts"), None),
            Scope {
                prefix: String::new(),
                glob: Some("**/*.ts".into())
            }
        );
        assert_eq!(
            Scope::resolve(root, Some("./src"), Some("*.rs")),
            Scope {
                prefix: "src".into(),
                glob: Some("*.rs".into())
            }
        );
    }

    #[test]
    fn paths_normalize_to_project_relative_prefixes() {
        let root = Path::new("/work/proj");
        assert_eq!(normalize_path(root, "./src/"), "src");
        assert_eq!(normalize_path(root, "."), "");
        assert_eq!(normalize_path(root, ""), "");
        assert_eq!(normalize_path(root, "/work/proj/src/lib.rs"), "src/lib.rs");
        assert_eq!(normalize_path(root, "/work/proj"), "");
        assert_eq!(normalize_path(root, "/work/project2/x"), "work/project2/x");
        assert_eq!(normalize_path(root, "src\\mcp"), "src/mcp");
    }
}

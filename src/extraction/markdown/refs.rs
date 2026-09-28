//! What a document refers to, as reference names the document-link pass
//! (`links.rs`) resolves: `rfc:<n>` (an RFC by number), `doc:<path>[#anchor]`
//! (a Markdown link to another document, path relative to the project root)
//! and `feature:<name>` (a Rust feature gate). Rust sources emit `feature:`
//! too, for `#![feature(..)]`.
//!
//! Every pattern runs once over the whole text; a match's line comes from
//! `LineStarts`, never from rescanning the text before it.

use std::sync::LazyLock;

use regex::Regex;

/// Prefix of a reference to an RFC by number.
pub const RFC_PREFIX: &str = "rfc:";
/// Prefix of a reference to another document by path.
pub const DOC_PREFIX: &str = "doc:";
/// Prefix of a reference to a Rust feature gate.
pub const FEATURE_PREFIX: &str = "feature:";

/// `RFC 2585`, `RFC #2585`, `RFC-2585`, `RFC2585`, `RFCs 1238`.
static RFC_TEXT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bRFCs?[ \t]*[-#]?[ \t]*(\d{1,4})\b").expect("valid regex"));
/// `rust-lang/rfcs#2585`, and links to the RFC's PR, its text in the repo,
/// or its rendered page in the RFC book.
static RFC_REPO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"rust-lang/rfcs(?:#|/pull/|/blob/[^/\s)]+/text/)(\d{1,5})\b|rust-lang\.github\.io/rfcs/(\d{1,5})-",
    )
    .expect("valid regex")
});
/// An inline link's target: `[text](target "title")`.
static INLINE_LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\]\(\s*<?([^)\s>]+)>?(?:\s+"[^"]*")?\s*\)"#).expect("valid regex")
});
/// A link reference definition: `[label]: target`.
static LINK_DEFINITION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^[ \t]{0,3}\[[^\]\n]+\]:[ \t]*<?([^\s>]+)>?").expect("valid regex")
});
/// `feature(a, b)` — `#![feature(..)]`, `cfg_attr(.., feature(..))`, prose.
/// `feature = "x"` (a Cargo feature) does not match.
static FEATURE_CALL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\bfeature\s*\(\s*([A-Za-z_][A-Za-z0-9_]*(?:\s*,\s*[A-Za-z_][A-Za-z0-9_]*)*)\s*,?\s*\)",
    )
    .expect("valid regex")
});
/// "feature gate `x`", "feature-gate `x`", "`x` feature gate".
static FEATURE_GATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i:feature[- ]gates?)[ \t]+(?:(?:named|called)[ \t]+)?`([A-Za-z_][A-Za-z0-9_]*)`|`([A-Za-z_][A-Za-z0-9_]*)`[ \t]+(?i:feature[- ]gate)",
    )
    .expect("valid regex")
});
/// `text/2585-unsafe-block-in-unsafe-fn.md`: the rust-lang/rfcs naming.
static RFC_FILE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d{4,5})-([a-z0-9][a-z0-9_-]*)\.md$").expect("valid regex"));
/// `- Feature Name: \`x\``, in an RFC's header list.
static FEATURE_NAME_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^[ \t]*[-*][ \t]*Feature[ \t]+[Nn]ames?:[ \t]*(.*)$").expect("valid regex")
});
static IDENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").expect("valid regex"));
static BACKTICKED_IDENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"`([A-Za-z_][A-Za-z0-9_]*)`").expect("valid regex"));

/// One reference found in a document: its name and where it starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Found {
    pub name: String,
    pub offset: usize,
}

/// The RFC a file is, by the rust-lang/rfcs naming (`NNNN-slug.md`): its
/// number and slug. `0000-…` (an unnumbered proposal) is not an RFC yet.
pub(crate) fn rfc_of_file(file_path: &str) -> Option<(u32, &str)> {
    let base = file_path.rsplit('/').next()?;
    let caps = RFC_FILE.captures(base)?;
    let number: u32 = caps.get(1)?.as_str().parse().ok()?;
    (number > 0).then(|| (number, caps.get(2).map_or("", |m| m.as_str())))
}

/// The feature gates an RFC's header declares (`- Feature Name: \`x\``),
/// with the byte offset of each, from the first `header_end` bytes.
pub(super) fn declared_features(source: &str, header_end: usize) -> Vec<(String, usize)> {
    let header = &source[..header_end.min(source.len())];
    let mut features = Vec::new();
    for caps in FEATURE_NAME_LINE.captures_iter(header) {
        let value = caps.get(1).expect("group 1");
        let text = value.as_str().trim();
        let mut named: Vec<(String, usize)> = BACKTICKED_IDENT
            .captures_iter(text)
            .map(|c| {
                let m = c.get(1).expect("group 1");
                (m.as_str().to_string(), value.start() + m.start())
            })
            .collect();
        if named.is_empty() {
            for part in text.split(|c: char| c == ',' || c.is_whitespace()) {
                let part = part.trim_matches(|c: char| c == '(' || c == ')' || c == '.');
                if IDENT.is_match(part) && !is_placeholder(part) {
                    let at = value.start() + text.find(part).unwrap_or(0);
                    named.push((part.to_string(), at));
                }
            }
        }
        for (name, at) in named {
            if !is_placeholder(&name) && !features.iter().any(|(n, _)| *n == name) {
                features.push((name, at));
            }
        }
    }
    features
}

fn is_placeholder(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "n" | "a"
            | "na"
            | "none"
            | "fill"
            | "me"
            | "in"
            | "with"
            | "unique"
            | "ident"
            | "tbd"
            | "my_awesome_feature"
            | "not"
            | "applicable"
            | "the"
            | "and"
    )
}

/// Every reference in `source`, in no particular order: RFC numbers,
/// Markdown links to other documents, and feature gates. `file_path` is the
/// document's project-relative path (links resolve against its directory);
/// `own_rfc` is left out (a document's header links to itself).
pub(super) fn find_references(source: &str, file_path: &str, own_rfc: Option<u32>) -> Vec<Found> {
    let mut found = Vec::new();
    let rfc = |number: &str, offset: usize, found: &mut Vec<Found>| {
        if let Ok(number) = number.parse::<u32>() {
            if number > 0 && Some(number) != own_rfc {
                found.push(Found {
                    name: format!("{RFC_PREFIX}{number}"),
                    offset,
                });
            }
        }
    };
    for caps in RFC_TEXT.captures_iter(source) {
        let whole = caps.get(0).expect("match");
        rfc(&caps[1], whole.start(), &mut found);
    }
    for caps in RFC_REPO.captures_iter(source) {
        let whole = caps.get(0).expect("match");
        let number = caps.get(1).or_else(|| caps.get(2)).expect("one group");
        rfc(number.as_str(), whole.start(), &mut found);
    }
    for regex in [&*INLINE_LINK, &*LINK_DEFINITION] {
        for caps in regex.captures_iter(source) {
            let target = caps.get(1).expect("group 1");
            if let Some(name) = document_link(file_path, target.as_str()) {
                found.push(Found {
                    name,
                    offset: target.start(),
                });
            }
        }
    }
    for caps in FEATURE_CALL.captures_iter(source) {
        let list = caps.get(1).expect("group 1");
        for name in list.as_str().split(',') {
            let name = name.trim();
            if IDENT.is_match(name) {
                found.push(Found {
                    name: format!("{FEATURE_PREFIX}{name}"),
                    offset: list.start(),
                });
            }
        }
    }
    for caps in FEATURE_GATE.captures_iter(source) {
        let name = caps.get(1).or_else(|| caps.get(2)).expect("one group");
        found.push(Found {
            name: format!("{FEATURE_PREFIX}{}", name.as_str()),
            offset: name.start(),
        });
    }
    found
}

/// The feature gates a Rust inner attribute enables: `#![feature(a, b)]`,
/// also inside `cfg_attr(…, feature(…))`.
pub(crate) fn rust_attribute_features(attribute: &str) -> Vec<String> {
    let mut names = Vec::new();
    for caps in FEATURE_CALL.captures_iter(attribute) {
        for name in caps[1].split(',') {
            let name = name.trim();
            if IDENT.is_match(name) && !names.iter().any(|n| n == name) {
                names.push(name.to_string());
            }
        }
    }
    names
}

/// `doc:<path>[#anchor]` for a relative link to a Markdown document, or
/// `rfc:<n>` for a link to an RFC's text by URL. Dots in the path are
/// written `%2E`, so the name matcher's `receiver.member` split never sees
/// a file extension as a member name.
fn document_link(file_path: &str, target: &str) -> Option<String> {
    if target.contains("://") || target.starts_with("mailto:") || target.starts_with('#') {
        return None;
    }
    let (path, anchor) = match target.split_once('#') {
        Some((path, anchor)) => (path, Some(anchor)),
        None => (target, None),
    };
    let path = path.split('?').next().unwrap_or(path).replace("%20", " ");
    let lower = path.to_ascii_lowercase();
    if !(lower.ends_with(".md") || lower.ends_with(".markdown")) {
        return None;
    }
    let resolved = resolve_relative(file_path, &path)?;
    let mut name = format!("{DOC_PREFIX}{}", resolved.replace('.', "%2E"));
    if let Some(anchor) = anchor.filter(|a| !a.is_empty() && a.len() <= 200) {
        name.push('#');
        name.push_str(&anchor.replace('.', "%2E"));
    }
    Some(name)
}

/// `target` (relative to `file_path`'s directory, or to the root when it
/// starts with `/`) as a project-relative path; `None` when it leaves the
/// project.
fn resolve_relative(file_path: &str, target: &str) -> Option<String> {
    let mut parts: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        let mut dir: Vec<&str> = file_path.split('/').collect();
        dir.pop();
        dir
    };
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Decode a `doc:` reference name into `(path, anchor)`.
pub(crate) fn decode_document_link(name: &str) -> Option<(String, Option<String>)> {
    let rest = name.strip_prefix(DOC_PREFIX)?;
    let (path, anchor) = match rest.split_once('#') {
        Some((path, anchor)) => (path, Some(anchor.replace("%2E", "."))),
        None => (rest, None),
    };
    Some((path.replace("%2E", "."), anchor))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(source: &str, path: &str, own: Option<u32>) -> Vec<String> {
        let mut names: Vec<String> = find_references(source, path, own)
            .into_iter()
            .map(|f| f.name)
            .collect();
        names.sort();
        names.dedup();
        names
    }

    #[test]
    fn rfc_numbers_in_every_spelling() {
        let source = "See RFC 2585, RFC #1238 and RFC-560, rust-lang/rfcs#1857, \
            [x](https://github.com/rust-lang/rfcs/pull/2349), \
            [y](https://github.com/rust-lang/rfcs/blob/master/text/1940-must-use-functions.md), \
            [z](https://rust-lang.github.io/rfcs/2945-c-unwind-abi.html). Not RFCish 12.";
        assert_eq!(
            names(source, "text/0001-x.md", None),
            vec![
                "rfc:1238", "rfc:1857", "rfc:1940", "rfc:2349", "rfc:2585", "rfc:2945", "rfc:560"
            ]
        );
        // A document's own number is its header, not a reference.
        assert_eq!(
            names("RFC 2585 and RFC 1", "text/2585-x.md", Some(2585)),
            vec!["rfc:1"]
        );
    }

    #[test]
    fn links_to_documents_resolve_against_the_file() {
        let source = "[a](0001-private-fields.md) [b](../README.md#rfc-process)\n\
            [c]: ./sub/d.md\n[e](https://example.com/x.md) [f](#local) [g](image.png) [h](../../out.md)";
        assert_eq!(
            names(source, "text/2585-x.md", None),
            vec![
                "doc:README%2Emd#rfc-process",
                "doc:text/0001-private-fields%2Emd",
                "doc:text/sub/d%2Emd"
            ]
        );
        assert_eq!(
            decode_document_link("doc:README%2Emd#rfc-process"),
            Some(("README.md".to_string(), Some("rfc-process".to_string())))
        );
    }

    #[test]
    fn feature_gates_but_not_cargo_features() {
        let source = "#![feature(never_type, try_blocks)]\n\
            the feature gate `dropck_eyepatch` and the `generic_associated_types` feature gate\n\
            #[cfg(feature = \"std\")] the `std` feature";
        assert_eq!(
            names(source, "a.md", None),
            vec![
                "feature:dropck_eyepatch",
                "feature:generic_associated_types",
                "feature:never_type",
                "feature:try_blocks"
            ]
        );
        assert_eq!(
            rust_attribute_features("#![cfg_attr(nightly, feature(never_type))]"),
            vec!["never_type"]
        );
    }

    #[test]
    fn rfc_headers_declare_numbers_and_features() {
        assert_eq!(
            rfc_of_file("text/1238-nonparametric-dropck.md"),
            Some((1238, "nonparametric-dropck"))
        );
        assert_eq!(rfc_of_file("text/0000-template.md"), None);
        assert_eq!(rfc_of_file("README.md"), None);
        let header = "- Feature Name: `dropck_eyepatch`, `may_dangle`\n- Start Date: 2015\n";
        let names: Vec<String> = declared_features(header, header.len())
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(names, vec!["dropck_eyepatch", "may_dangle"]);
        let plain = "- Feature Name: unsafe_block_in_unsafe_fn\n";
        assert_eq!(
            declared_features(plain, plain.len())[0].0,
            "unsafe_block_in_unsafe_fn"
        );
        let none = "- Feature Name: (fill me in with a unique ident, `my_awesome_feature`)\n- Feature Name: N/A\n";
        assert!(declared_features(none, none.len()).is_empty());
    }
}

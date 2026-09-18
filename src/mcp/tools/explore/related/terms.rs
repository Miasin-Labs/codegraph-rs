//! Query-term overlap: how many of the query's words a path or a set of
//! symbol names contains, compared as lowercase identifier segments.

use std::collections::HashSet;

use crate::search::extract_search_terms_opts;

/// Lowercase query words (stop words dropped, identifiers split into their
/// segments, compounds kept whole), without stem variants.
pub(super) fn query_terms(query: &str) -> HashSet<String> {
    extract_search_terms_opts(query, false)
        .into_iter()
        .collect()
}

/// Lowercase identifier segments of `text`: split on every non-alphanumeric
/// character and at camelCase / acronym boundaries (`parseHTTPRequest` →
/// `parse`, `http`, `request`). Each whole run is kept too, lowercased, so a
/// compound query term (`scrapeloop`) still matches `scrapeLoop`.
pub(super) fn segments(text: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    for run in text.split(|c: char| !c.is_ascii_alphanumeric()) {
        if run.is_empty() {
            continue;
        }
        out.insert(run.to_ascii_lowercase());
        let chars: Vec<char> = run.chars().collect();
        let mut start = 0;
        for i in 1..chars.len() {
            let (prev, cur) = (chars[i - 1], chars[i]);
            let next_lower = chars.get(i + 1).is_some_and(char::is_ascii_lowercase);
            let boundary = (prev.is_ascii_lowercase() && cur.is_ascii_uppercase())
                || (prev.is_ascii_uppercase() && cur.is_ascii_uppercase() && next_lower)
                || (prev.is_ascii_digit() != cur.is_ascii_digit());
            if boundary {
                out.insert(
                    chars[start..i]
                        .iter()
                        .collect::<String>()
                        .to_ascii_lowercase(),
                );
                start = i;
            }
        }
        out.insert(
            chars[start..]
                .iter()
                .collect::<String>()
                .to_ascii_lowercase(),
        );
    }
    out.retain(|segment| segment.len() >= 3);
    out
}

/// Number of `terms` present among `segments`.
pub(super) fn overlap(terms: &HashSet<String>, segments: &HashSet<String>) -> usize {
    terms.iter().filter(|term| segments.contains(*term)).count()
}

//! Identifier words materialized for prompt-to-graph matching.

use std::collections::HashSet;

const MIN_SEGMENT_CHARS: usize = 2;
const MAX_SEGMENT_CHARS: usize = 32;
const MAX_SEGMENTS_PER_NAME: usize = 12;

fn push_segment(raw: &[char], out: &mut Vec<String>, seen: &mut HashSet<String>) {
    if raw.is_empty() || out.len() >= MAX_SEGMENTS_PER_NAME {
        return;
    }
    let segment: String = raw.iter().flat_map(|ch| ch.to_lowercase()).collect();
    let len = segment.chars().count();
    if !(MIN_SEGMENT_CHARS..=MAX_SEGMENT_CHARS).contains(&len)
        || segment.chars().all(|ch| ch.is_numeric())
        || !seen.insert(segment.clone())
    {
        return;
    }
    out.push(segment);
}

fn split_run(run: &[char], out: &mut Vec<String>, seen: &mut HashSet<String>) {
    let mut start = 0usize;
    for index in 1..run.len() {
        let previous = run[index - 1];
        let current = run[index];
        let next = run.get(index + 1).copied();
        let lower_or_digit_to_upper =
            (previous.is_lowercase() || previous.is_numeric()) && current.is_uppercase();
        let acronym_to_word = previous.is_uppercase()
            && current.is_uppercase()
            && next.is_some_and(char::is_lowercase);
        if lower_or_digit_to_upper || acronym_to_word {
            push_segment(&run[start..index], out, seen);
            if out.len() >= MAX_SEGMENTS_PER_NAME {
                return;
            }
            start = index;
        }
    }
    push_segment(&run[start..], out, seen);
}

/// Split camelCase, acronym runs, snake/kebab/dotted names, and Unicode
/// alphanumeric identifiers into the lowercase words people use in prose.
pub fn split_identifier_segments(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut run = Vec::new();

    for ch in name.chars().chain(std::iter::once('\0')) {
        if ch.is_alphanumeric() {
            run.push(ch);
            continue;
        }
        split_run(&run, &mut out, &mut seen);
        run.clear();
        if out.len() >= MAX_SEGMENTS_PER_NAME {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_humps_acronyms_and_delimiters() {
        assert_eq!(
            split_identifier_segments("OrderStateMachine"),
            ["order", "state", "machine"]
        );
        assert_eq!(
            split_identifier_segments("HTMLParser_base64Encode.ts"),
            ["html", "parser", "base64", "encode", "ts"]
        );
    }

    #[test]
    fn drops_tiny_numeric_and_duplicate_segments() {
        assert_eq!(split_identifier_segments("x_state-state_123"), ["state"]);
    }
}

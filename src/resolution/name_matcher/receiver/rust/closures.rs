//! The type of an untyped closure parameter, from the method the closure
//! is passed to: `nodes.iter().filter(|n| …)` hands `n` an item of
//! `nodes`, `slot.as_ref().is_some_and(|graph| …)` the value in the
//! `Option`, `v.sort_by(|a, b| …)` two elements.
//!
//! Only a closure written as the method's first argument is typed, and
//! only for the methods [`CLOSURE_METHODS`] lists (iterator adaptors,
//! `Option`/`Result` combinators, `Vec` callbacks), all of which hand the
//! closure the receiver's items. The receiver is read back from the text
//! before the closure ([`receiver_before`]) and typed like any chain.

use super::{Inference, MAX_LINE_BYTES, Site, Value};

/// How many lines above the closure a multi-line receiver chain may start.
const MAX_CHAIN_LINES: usize = 8;

/// Methods whose first-argument closure takes the receiver's items: one
/// item, or two for the comparators (`sort_by`, `max_by`).
const CLOSURE_METHODS: &[&str] = &[
    "all",
    "and_then",
    "any",
    "dedup_by",
    "dedup_by_key",
    "filter",
    "filter_map",
    "find",
    "find_map",
    "flat_map",
    "for_each",
    "inspect",
    "is_none_or",
    "is_ok_and",
    "is_some_and",
    "is_sorted_by",
    "map",
    "map_while",
    "max_by",
    "max_by_key",
    "min_by",
    "min_by_key",
    "partition",
    "position",
    "retain",
    "retain_mut",
    "rposition",
    "skip_while",
    "sort_by",
    "sort_by_cached_key",
    "sort_by_key",
    "sort_unstable_by",
    "sort_unstable_by_key",
    "take_if",
    "take_while",
    "try_for_each",
];

impl Inference<'_> {
    /// The type of parameter `param` (element `position` of it, when it
    /// is a tuple pattern) of the closure opening at byte `open` of line
    /// `index`.
    pub(super) fn closure_param_value(
        &self,
        index: usize,
        open: usize,
        param: usize,
        position: Option<usize>,
        depth: u8,
    ) -> Option<Value> {
        let first = index
            .saturating_sub(MAX_CHAIN_LINES)
            .max(self.first_line)
            .min(index);
        let above = &self.lines[first..index];
        if above.iter().any(|line| line.len() > MAX_LINE_BYTES) {
            return None;
        }
        let mut before = above.join("\n");
        before.push('\n');
        before.push_str(self.lines[index].get(..open)?);
        let (receiver, method) = receiver_before(&before)?;
        if !CLOSURE_METHODS.contains(&method) || param > usize::from(is_comparator(method)) {
            return None;
        }
        let site = Site {
            line: Some(index),
            column: Some(open),
        };
        let receiver = self.expression_value(receiver, site, depth + 1)?;
        let item = self.iterated_value(receiver)?;
        match position {
            Some(position) => self.tuple_element(item, position),
            None => Some(item),
        }
    }
}

fn is_comparator(method: &str) -> bool {
    matches!(
        method,
        "dedup_by" | "is_sorted_by" | "max_by" | "min_by" | "sort_by" | "sort_unstable_by"
    )
}

/// The call a closure is the first argument of, read back from `text` (all
/// that is written before the closure's `|`): its receiver expression and
/// method, `(recv, m)` for `recv.m(move |`.
pub(super) fn receiver_before(text: &str) -> Option<(&str, &str)> {
    let text = text.trim_end();
    let text = text.strip_suffix("move").map_or(text, str::trim_end);
    let text = text.strip_suffix('(')?.trim_end();
    let name_start = text
        .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .map_or(0, |at| at + 1);
    let method = &text[name_start..];
    let before = text[..name_start].trim_end().strip_suffix('.')?.trim_end();
    let start = expression_start(before)?;
    let receiver = before[start..].trim();
    (!method.is_empty() && !receiver.is_empty()).then_some((receiver, method))
}

/// Where the postfix expression ending `text` starts: back over
/// identifiers, `.`, `::`, `?`, and balanced `(..)`, `[..]`, `<..>` groups,
/// until something that cannot be part of it (`=`, `,`, an opening bracket
/// left open, an operator, a keyword before a space).
fn expression_start(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut index = bytes.len();
    loop {
        // Whitespace (a chain split over lines) may only precede a `.`.
        let trimmed = text[..index].trim_end();
        let next = bytes.get(index).copied();
        if trimmed.len() < index && next != Some(b'.') {
            return Some(index);
        }
        index = trimmed.len();
        let Some(&byte) = bytes.get(index.checked_sub(1)?) else {
            return Some(index);
        };
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'.' | b'?' => index -= 1,
            b':' if index >= 2 && bytes[index - 2] == b':' => index -= 2,
            b')' | b']' | b'>' => index = opening(bytes, index - 1)?,
            _ => return Some(index),
        }
        if index == 0 {
            return Some(0);
        }
    }
}

/// The index of the bracket opening the one closing at `close`.
fn opening(bytes: &[u8], close: usize) -> Option<usize> {
    let (open, shut) = match bytes[close] {
        b')' => (b'(', b')'),
        b']' => (b'[', b']'),
        _ => (b'<', b'>'),
    };
    let mut depth = 0usize;
    for index in (0..=close).rev() {
        if bytes[index] == shut {
            depth += 1;
        } else if bytes[index] == open {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::receiver_before;

    #[test]
    fn reads_the_call_a_closure_is_passed_to() {
        assert_eq!(
            receiver_before("    let kept = self.items.iter().filter("),
            Some(("self.items.iter()", "filter"))
        );
        assert_eq!(
            receiver_before("assert!(cur.iter().all("),
            Some(("cur.iter()", "all"))
        );
        assert_eq!(
            receiver_before("    self.nodes\n        .values()\n        .find(move "),
            Some(("self.nodes\n        .values()", "find"))
        );
        assert_eq!(
            receiver_before("x = a.b::<Vec<u8>>().c(i)[0].map("),
            Some(("a.b::<Vec<u8>>().c(i)[0]", "map"))
        );
        assert_eq!(receiver_before("fold(0, "), None);
        assert_eq!(receiver_before("let f = "), None);
        assert_eq!(receiver_before("spawn("), None);
    }
}

//! What the flagged code's text says that the lint does not.
//!
//! The input-exposure lints fire on every shape (`unwrap_used` on every
//! `unwrap`), and reviewing them on rms and codegraph-rs showed a few
//! shapes that are never the bug the lint is there for. Each rule here reads
//! only the primary span's text and the lint's message, and was kept only
//! where every reviewed finding of that shape was a false positive:
//!
//! - an unwrap of a lock (`.lock().unwrap()`): it panics only after another
//!   thread panicked holding the lock — no input chooses that;
//! - an unwrap of a value built from literals (`"4".parse().unwrap()`,
//!   `Regex::new(r"…").unwrap()`): it fails the same way on every run;
//! - `x += 1`: a counter bumped once per item cannot overflow before memory
//!   runs out;
//! - a truncating cast clippy qualifies as "on targets with 32-bit wide
//!   pointers": not a bug on the 64-bit hosts servers run on (discounted,
//!   not dropped);
//! - addition (not subtraction or multiplication) and `x["literal"]` lookups
//!   (usually a `serde_json::Value`, whose index returns `Null`): discounted.

use std::sync::LazyLock;

use regex::Regex;

/// The verdict on a finding's shape.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shape {
    /// Nothing to add.
    Keep,
    /// Multiply the confidence (with the reason).
    Discount(f64, &'static str),
    /// Not a finding (with the reason).
    Drop(&'static str),
}

static LOCK_UNWRAP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\.(?:lock|read|write)\(\)\s*\.\s*(?:unwrap\(\)|expect\()").expect("valid regex")
});

/// A receiver made only of literals: `"4".parse()`, `"x".parse::<T>()`,
/// `Path::fn("lit")`, `Path::fn(r#"lit"#)`, `Path::fn(3)`.
static LITERAL_UNWRAP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r##"^(?:r?#*"[^"]*"#*\s*\.\s*parse(?:::<[^>]*>)?\(\)|[A-Za-z_][\w:<>]*\(\s*(?:r?#*"[^"]*"#*|b"[^"]*"|\d[\w.]*)\s*\))\s*\.\s*(?:unwrap\(\)|expect\()"##,
    )
    .expect("valid regex")
});

static INCREMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[\w.\[\]()]+\s*\+=\s*1(?:_?[ui](?:8|16|32|64|128|size))?$").expect("valid regex")
});

static STRING_KEY_INDEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\[\s*"[^"]*"\s*\]$"#).expect("valid regex"));

/// Judge a finding of lint `name` (without the `clippy::` prefix) from its
/// message and its primary span's text (whitespace as in the source).
pub fn judge(name: &str, message: &str, text: &str) -> Shape {
    let text = text.trim();
    match name {
        "unwrap_used" | "expect_used" | "unwrap_in_result" => {
            if LOCK_UNWRAP.is_match(text) {
                return Shape::Drop(
                    "unwraps a lock: panics only if another thread panicked holding it",
                );
            }
            if LITERAL_UNWRAP.is_match(text) {
                return Shape::Drop(
                    "unwraps a value built from literals: fails on every run, not on input",
                );
            }
            Shape::Keep
        }
        "arithmetic_side_effects" => {
            let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if INCREMENT.is_match(&flat) {
                return Shape::Drop(
                    "a counter bumped by one cannot overflow before memory runs out",
                );
            }
            let operators = operators(&flat);
            if !operators.is_empty() && operators.iter().all(|op| *op == '+') {
                return Shape::Discount(0.5, "addition of in-memory sizes rarely overflows");
            }
            Shape::Keep
        }
        "cast_possible_truncation" if message.contains("32-bit wide pointers") => {
            Shape::Discount(0.3, "truncates only on 32-bit targets")
        }
        "indexing_slicing" if STRING_KEY_INDEX.is_match(text) => Shape::Discount(
            0.5,
            "a string-keyed lookup, often a `serde_json::Value` (whose index returns Null)",
        ),
        _ => Shape::Keep,
    }
}

/// The binary arithmetic operators outside string and char literals.
fn operators(text: &str) -> Vec<char> {
    let mut out = Vec::new();
    let mut in_string = false;
    let mut prev = ' ';
    let bytes: Vec<char> = text.chars().collect();
    for (i, &c) in bytes.iter().enumerate() {
        if c == '"' && prev != '\\' {
            in_string = !in_string;
        }
        if !in_string && matches!(c, '+' | '-' | '*' | '/' | '%') {
            let next = bytes.get(i + 1).copied().unwrap_or(' ');
            // `->`, `//`, `*` derefs and unary minus are not arithmetic.
            let unary = matches!(prev, ' ' | '(' | '[' | ',' | '=') && next != ' ' && c != '+';
            if !(c == '-' && next == '>') && !(c == '/' && next == '/') && !unary {
                out.push(c);
            }
        }
        prev = c;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_and_literal_unwraps_are_dropped() {
        let drop = |text| matches!(judge("unwrap_used", "", text), Shape::Drop(_));
        assert!(drop("self.inner.db.lock().unwrap()"));
        assert!(drop("state.cache.write().expect(\"poisoned\")"));
        assert!(drop("\"4\".parse().unwrap()"));
        assert!(drop("\"crdt,sharing\".parse::<HeaderValue>().unwrap()"));
        assert!(drop("Regex::new(r\"^[a-f0-9]{64}$\").unwrap()"));
        assert!(!drop(
            "DateTime::parse_from_rfc3339(&row.get::<_, String>(9)?).unwrap()"
        ));
        assert!(!drop("body.parse().unwrap()"));
        assert!(!drop("Regex::new(&pattern).unwrap()"));
        assert!(!drop("file.read(&mut buf).unwrap()"));
    }

    #[test]
    fn counters_drop_additions_discount_and_subtractions_stay() {
        let judge = |text| judge("arithmetic_side_effects", "", text);
        assert!(matches!(judge("dropped += 1"), Shape::Drop(_)));
        assert!(matches!(judge("response.refreshed += 1"), Shape::Drop(_)));
        assert!(matches!(judge("total += hits.len()"), Shape::Discount(..)));
        assert!(matches!(
            judge("start + Duration::days(30)"),
            Shape::Discount(..)
        ));
        assert_eq!(judge("end - start + 1"), Shape::Keep);
        assert_eq!(judge("tokens.len() - 1"), Shape::Keep);
        assert_eq!(judge("width * height"), Shape::Keep);
        assert_eq!(judge("count -= 1"), Shape::Keep);
    }

    #[test]
    fn thirty_two_bit_truncation_and_string_keys_are_discounted() {
        assert!(matches!(
            judge(
                "cast_possible_truncation",
                "casting `u64` to `usize` may truncate the value on targets with 32-bit wide pointers",
                "(chunk.len() as u64).min(room) as usize"
            ),
            Shape::Discount(..)
        ));
        assert_eq!(
            judge(
                "cast_possible_truncation",
                "casting `u64` to `u32` may truncate the value",
                "x as u32"
            ),
            Shape::Keep
        );
        assert!(matches!(
            judge("indexing_slicing", "", "body[\"error\"]"),
            Shape::Discount(..)
        ));
        assert_eq!(judge("indexing_slicing", "", "parts[3]"), Shape::Keep);
    }
}

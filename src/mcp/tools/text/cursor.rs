//! Paging state of a grep query.
//!
//! Candidate files are searched in path order. A page covers a contiguous
//! range of them: an open page searches from `start` until the deadline or
//! the byte budget, a closed page re-searches exactly `start..end` (so its
//! ranking is the same as the page that handed out the cursor) and skips the
//! `skip` best-ranked files already listed.

use super::matcher::PatternFlags;
use crate::utils::sha256_hex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::mcp::tools) struct Cursor {
    /// First candidate file (index into the path-ordered candidates).
    pub start: usize,
    /// End of a page already searched once; `None` searches as far as the
    /// budget allows.
    pub end: Option<usize>,
    /// Ranked files of `start..end` already listed.
    pub skip: usize,
}

impl Cursor {
    pub const FIRST: Self = Self {
        start: 0,
        end: None,
        skip: 0,
    };

    pub fn encode(self, key: &str) -> String {
        let end = self
            .end
            .map_or_else(|| "-".to_string(), |end| end.to_string());
        format!("g1.{}.{end}.{}.{key}", self.start, self.skip)
    }

    /// Parse a cursor, or `None` when it is malformed or belongs to another
    /// query.
    pub fn decode(raw: &str, key: &str) -> Option<Self> {
        let mut parts = raw.trim().split('.');
        if parts.next()? != "g1" {
            return None;
        }
        let start = parts.next()?.parse().ok()?;
        let end = match parts.next()? {
            "-" => None,
            end => Some(end.parse().ok()?),
        };
        let skip = parts.next()?.parse().ok()?;
        if parts.next()? != key || parts.next().is_some() {
            return None;
        }
        if end.is_some_and(|end| end < start) {
            return None;
        }
        Some(Self { start, end, skip })
    }
}

/// Identity of the search a cursor pages through: everything that decides
/// which files are candidates and which lines match.
pub(in crate::mcp::tools) fn query_key(
    pattern: &str,
    flags: PatternFlags,
    path: &str,
    glob: &str,
) -> String {
    let PatternFlags {
        literal,
        case_insensitive,
        word,
    } = flags;
    let identity =
        format!("{pattern}\u{0}{literal}\u{0}{case_insensitive}\u{0}{word}\u{0}{path}\u{0}{glob}");
    sha256_hex(identity.as_bytes())[..10].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_rejects_foreign_cursors() {
        let key = query_key("foo", PatternFlags::default(), "src", "");
        for cursor in [
            Cursor::FIRST,
            Cursor {
                start: 40,
                end: Some(90),
                skip: 12,
            },
        ] {
            assert_eq!(Cursor::decode(&cursor.encode(&key), &key), Some(cursor));
        }
        let other = query_key(
            "foo",
            PatternFlags {
                case_insensitive: true,
                ..Default::default()
            },
            "src",
            "",
        );
        assert_ne!(key, other);
        let word = PatternFlags {
            word: true,
            ..Default::default()
        };
        assert_ne!(key, query_key("foo", word, "src", ""));
        let cursor = Cursor::FIRST.encode(&key);
        assert_eq!(Cursor::decode(&cursor, &other), None);
        assert_eq!(Cursor::decode("garbage", &key), None);
        assert_eq!(Cursor::decode(&format!("g1.9.3.0.{key}"), &key), None);
        assert_eq!(Cursor::decode(&format!("{cursor}.extra"), &key), None);
    }
}

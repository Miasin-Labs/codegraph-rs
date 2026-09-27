//! The language-neutral model of a fuzz target: what a function takes (as
//! input shapes a fuzzer can produce), what it risks (sites in its body and
//! its callees), and the score that ranks it.
//!
//! An ecosystem (`super::rust`) classifies its own parameter types into
//! [`InputShape`]s and says how to reach a function from outside the
//! package; everything here — features, scoring, the report rows — is shared.

use serde::Serialize;

/// What a fuzzer has to produce for one parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum InputShape {
    /// Raw bytes (`&[u8]`, `Vec<u8>`, `impl AsRef<[u8]>`).
    Bytes,
    /// A byte stream (`impl Read`, `R: BufRead`, `&mut dyn Read`).
    Reader,
    /// Text (`&str`, `String`, `impl AsRef<str>`).
    Text,
    /// A value a structured fuzzer builds (integers, floats, `bool`, `char`,
    /// and containers, tuples, arrays of them — `Arbitrary` in Rust).
    Structured,
    /// A value the harness fills with `Default::default()` (an options
    /// struct): not fuzzed, but no longer in the way.
    Fixed,
    /// Something the harness cannot build on its own (a project type with
    /// no usable constructor, a callback, a context object).
    Opaque,
}

impl InputShape {
    /// How much a fuzzer gains from this parameter alone: unstructured bytes
    /// and text reach parsers directly; scalars steer less code.
    pub fn weight(self) -> f64 {
        match self {
            Self::Bytes | Self::Reader => 4.0,
            Self::Text => 3.5,
            Self::Structured => 1.5,
            Self::Fixed | Self::Opaque => 0.0,
        }
    }

    /// Bytes, a reader or text: one fuzzer buffer feeds it directly.
    pub fn is_data(self) -> bool {
        matches!(self, Self::Bytes | Self::Reader | Self::Text)
    }
}

/// One parameter as the harness sees it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParamInfo {
    pub name: String,
    /// The type as written in the signature.
    #[serde(rename = "type")]
    pub ty: String,
    pub shape: InputShape,
}

/// Sites in one function's own body that a fuzzer can make go wrong.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteCounts {
    /// `unsafe` blocks (memory errors a sanitizer reports).
    pub unsafe_blocks: u32,
    /// Indexing and slicing (out-of-bounds panics).
    pub index: u32,
    /// Explicit panics: `unwrap`/`expect`, `panic!`, `unreachable!`,
    /// `assert!`.
    pub panics: u32,
    /// Arithmetic that can overflow (`+ - * <<` and their assignments;
    /// fuzz builds keep overflow checks on).
    pub arithmetic: u32,
    /// Loops (hangs and unbounded work).
    pub loops: u32,
}

impl SiteCounts {
    pub fn add(&mut self, other: &SiteCounts) {
        self.unsafe_blocks += other.unsafe_blocks;
        self.index += other.index;
        self.panics += other.panics;
        self.arithmetic += other.arithmetic;
        self.loops += other.loops;
    }

    /// One risk number: an `unsafe` block is worth three panic sites.
    pub fn risk(&self) -> f64 {
        3.0 * f64::from(self.unsafe_blocks)
            + f64::from(self.index)
            + f64::from(self.panics)
            + 0.5 * f64::from(self.arithmetic)
            + 0.5 * f64::from(self.loops)
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Why a function ranks where it does. Every field feeds [`score`].
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Features {
    /// The name says it parses or decodes (`parse`, `decode`, `from_*`…).
    pub parser_name: bool,
    /// Sites in the function's own body.
    pub own: SiteCounts,
    /// Sites in the function and everything it calls, discounted by call
    /// depth (0.6 per hop, 4 hops).
    pub reach_risk: f64,
    /// Functions reachable through calls (bounded).
    pub reachable: usize,
    /// Distinct callers in the project (non-test).
    pub fan_in: usize,
    /// It, or something it calls, is recursive (stack exhaustion).
    pub recursive: bool,
    /// Bug findings (`analyze bugs`/`rules`) in it or its callees.
    pub findings: usize,
    /// An existing fuzz target already calls it (directly) or reaches it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub covered_by: Option<String>,
}

/// How far a harness can get on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Harnessable {
    /// The generated harness compiles and runs as is.
    Complete,
    /// It compiles but leaves `todo!()` for values only the author can build.
    NeedsInput,
    /// Not callable from outside the package.
    No,
}

/// How a method gets its receiver.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiverInfo {
    /// The owner type.
    pub owner: String,
    /// The constructor the harness calls (`Owner::new`, `Default`…), if one
    /// was found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constructor: Option<String>,
}

/// The weight of what a callable takes: its best data parameter, less for
/// scalars only, near nothing when some parameter cannot be built.
pub fn input_weight(params: &[ParamInfo]) -> f64 {
    if params.iter().any(|p| p.shape == InputShape::Opaque) {
        return 0.25;
    }
    let best = params.iter().map(|p| p.shape.weight()).fold(0.0, f64::max);
    // No parameter a fuzzer drives: nothing to fuzz.
    if best == 0.0 { 0.2 } else { best }
}

/// The rank score. Multiplicative in what the harness can feed (input,
/// receiver) — a function a fuzzer cannot drive is not a target however
/// risky — and additive in the reasons to drive it.
pub fn score(input: f64, receiver: f64, features: &Features) -> f64 {
    let reasons = 1.0
        + if features.parser_name { 1.0 } else { 0.0 }
        + 0.6 * features.reach_risk.ln_1p()
        + 0.3 * (features.fan_in as f64).ln_1p()
        + if features.recursive { 0.8 } else { 0.0 }
        + features.findings.min(3) as f64;
    let covered = if features.covered_by.is_some() {
        0.4
    } else {
        1.0
    };
    input * receiver * reasons * covered
}

/// Name words that say the function turns outside data into values.
const PARSER_WORDS: &[&str] = &[
    "parse",
    "decode",
    "deserialize",
    "deserialise",
    "unmarshal",
    "unpack",
    "decompress",
    "inflate",
    "unescape",
    "tokenize",
    "tokenise",
    "lex",
    "scan",
    "read",
    "load",
    "verify",
    "validate",
    "extract",
    "open",
    "der",
    "ber",
    "from",
];

/// `parse_header`, `decodeFrame`, `from_slice`, `read_to_end`…: whole words
/// of a snake_case or camelCase name.
pub fn is_parser_name(name: &str) -> bool {
    split_words(name)
        .iter()
        .any(|word| PARSER_WORDS.contains(&word.as_str()))
}

fn split_words(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut previous_lower = false;
    for ch in name.chars() {
        if ch == '_' || ch == '-' {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            previous_lower = false;
            continue;
        }
        if ch.is_uppercase() && previous_lower && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        previous_lower = ch.is_lowercase() || ch.is_ascii_digit();
        current.extend(ch.to_lowercase());
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    fn param(shape: InputShape) -> ParamInfo {
        ParamInfo {
            name: "p".into(),
            ty: "T".into(),
            shape,
        }
    }

    #[test]
    fn parser_names_are_whole_words() {
        assert!(is_parser_name("parse"));
        assert!(is_parser_name("from_slice"));
        assert!(is_parser_name("decodeFrame"));
        assert!(is_parser_name("der_decode"));
        assert!(is_parser_name("read_to_end"));
        assert!(!is_parser_name("sparse_matrix"));
        assert!(!is_parser_name("reader_count"));
        assert!(!is_parser_name("encode"));
    }

    #[test]
    fn input_weight_prefers_data_and_zeroes_opaque() {
        assert_eq!(input_weight(&[param(InputShape::Bytes)]), 4.0);
        assert_eq!(
            input_weight(&[param(InputShape::Structured), param(InputShape::Text)]),
            3.5
        );
        assert_eq!(input_weight(&[param(InputShape::Structured)]), 1.5);
        assert_eq!(
            input_weight(&[param(InputShape::Bytes), param(InputShape::Opaque)]),
            0.25
        );
        assert_eq!(input_weight(&[]), 0.2);
        assert_eq!(input_weight(&[param(InputShape::Fixed)]), 0.2);
        assert_eq!(
            input_weight(&[param(InputShape::Text), param(InputShape::Fixed)]),
            3.5
        );
    }

    #[test]
    fn score_orders_by_reasons_and_discounts_coverage() {
        let plain = Features::default();
        let parser = Features {
            parser_name: true,
            ..Features::default()
        };
        let risky = Features {
            reach_risk: 20.0,
            recursive: true,
            findings: 1,
            ..Features::default()
        };
        let covered = Features {
            covered_by: Some("fuzz_parse".into()),
            ..risky.clone()
        };
        let base = score(4.0, 1.0, &plain);
        assert!(score(4.0, 1.0, &parser) > base);
        assert!(score(4.0, 1.0, &risky) > score(4.0, 1.0, &parser));
        assert!(score(4.0, 1.0, &covered) < score(4.0, 1.0, &risky));
        // Input dominates: a risky fn on scalars loses to a parser on bytes.
        assert!(score(1.5, 1.0, &Features::default()) < base);
        assert_eq!(score(0.0, 1.0, &risky), 0.0);
    }

    #[test]
    fn site_risk_weighs_unsafe_most() {
        let unsafe_only = SiteCounts {
            unsafe_blocks: 1,
            ..SiteCounts::default()
        };
        let index_only = SiteCounts {
            index: 1,
            ..SiteCounts::default()
        };
        assert!(unsafe_only.risk() > index_only.risk());
        let mut sum = unsafe_only;
        sum.add(&index_only);
        assert_eq!(sum.risk(), 4.0);
    }
}

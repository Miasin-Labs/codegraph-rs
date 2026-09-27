//! The rule file format, as written: YAML read with `serde_yaml_ng`, every
//! mapping `deny_unknown_fields` so a misspelt key is an error that names
//! the keys it could have been, with the line and column where it sits.
//!
//! A file holds one rule (a mapping), a list of rules, or several `---`
//! documents of either. Fields where the weggli-ruleset format takes one
//! value or a list (`check-patterns`, `regex`, `language`) take either.

use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;

use serde::de::value::{MapAccessDeserializer, SeqAccessDeserializer};
use serde::de::{self, Deserializer, IntoDeserializer, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};

/// How bad a match is, when it is a bug.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    #[serde(alias = "none")]
    Info,
    Low,
    #[default]
    Medium,
    High,
    Critical,
}

impl Severity {
    /// The confidence a finding of this severity ranks with, unless the
    /// rule sets `confidence`.
    pub fn confidence(self) -> f64 {
        match self {
            Severity::Info => 0.2,
            Severity::Low => 0.4,
            Severity::Medium => 0.6,
            Severity::High => 0.75,
            Severity::Critical => 0.85,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Low => "low",
            Severity::Medium => "medium",
            Severity::High => "high",
            Severity::Critical => "critical",
        }
    }
}

/// One rule.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct RuleSpec {
    pub id: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub severity: Option<Severity>,
    /// 0..=1; overrides the severity's default ranking confidence.
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Languages every pattern of the rule runs on, unless a pattern names
    /// its own. Default: `[c, cpp]` for weggli patterns; required for
    /// tree-sitter queries.
    #[serde(default)]
    pub language: Option<OneOrMany<String>>,
    #[serde(
        alias = "check patterns",
        alias = "check-pattern",
        alias = "check pattern"
    )]
    pub check_patterns: OneOrMany<PatternSpec>,
    #[serde(
        default,
        alias = "ignore patterns",
        alias = "ignore-pattern",
        alias = "ignore pattern"
    )]
    pub ignore_patterns: Option<OneOrMany<PatternSpec>>,
    /// Finding text; `{capture}` is replaced by the capture's code.
    #[serde(default)]
    pub message: Option<String>,
    /// Questions a reviewer should answer for a finding of this rule
    /// (added to its `analyze review` checklist).
    #[serde(default)]
    pub review: Vec<String>,
    #[serde(default)]
    pub examples: Option<ExamplesSpec>,
}

/// One check (or ignore) pattern: a weggli `pattern` or a tree-sitter
/// `query`, with constraints on what it captured.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct PatternSpec {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub language: Option<OneOrMany<String>>,
    /// weggli pattern (C/C++).
    #[serde(default)]
    pub pattern: Option<String>,
    /// tree-sitter query (any language with a grammar).
    #[serde(default)]
    pub query: Option<String>,
    /// `var=regex` (must match) or `var!=regex` (must not), weggli-ruleset
    /// style; `$` on the variable is optional.
    #[serde(default, alias = "regexes")]
    pub regex: Option<OneOrMany<String>>,
    /// Semantic predicates, all of which must hold.
    #[serde(default, rename = "where")]
    pub where_: Vec<PredicateSpec>,
    #[serde(default)]
    pub message: Option<String>,
    /// The capture a finding is reported at.
    #[serde(default)]
    pub at: Option<String>,
    /// At most one match per function.
    #[serde(default)]
    pub limit: bool,
    /// Different variables must capture different code.
    #[serde(default)]
    pub unique: bool,
}

/// One `where` entry. Exactly one test per entry: a capture test
/// (`capture` + one of `regex`, `not-regex`, `resolves-to`,
/// `not-resolves-to`), `enclosing-function`, or `inside`/`not-inside`
/// (optionally with `capture`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct PredicateSpec {
    #[serde(default)]
    pub capture: Option<String>,
    #[serde(default)]
    pub regex: Option<String>,
    #[serde(default)]
    pub not_regex: Option<String>,
    #[serde(default)]
    pub resolves_to: Option<String>,
    #[serde(default)]
    pub not_resolves_to: Option<String>,
    #[serde(default)]
    pub enclosing_function: Option<EnclosingSpec>,
    #[serde(default)]
    pub inside: Option<String>,
    #[serde(default)]
    pub not_inside: Option<String>,
}

/// Tests on the function the match sits in; all given must hold.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct EnclosingSpec {
    /// Some call in the function resolves to a name matching this.
    #[serde(default)]
    pub calls: Option<String>,
    /// No call in the function resolves to a name matching this.
    #[serde(default)]
    pub calls_not: Option<String>,
    /// The function's name or qualified name matches this.
    #[serde(default)]
    pub name_regex: Option<String>,
    /// The function is (or is not) test code.
    #[serde(default)]
    pub is_test: Option<bool>,
}

/// Snippets the rule must (`bad`) and must not (`good`) match.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExamplesSpec {
    #[serde(default)]
    pub bad: Vec<ExampleSpec>,
    #[serde(default)]
    pub good: Vec<ExampleSpec>,
}

/// An example: the code as a string, or a mapping with the code and how
/// to read it.
#[derive(Debug, Clone)]
pub struct ExampleSpec {
    pub code: String,
    /// Language to parse it as (default: the rule's first language).
    pub language: Option<String>,
    /// File path it is checked as (test detection reads it).
    pub file: Option<String>,
    /// What calls resolve to, for `resolves-to`/`calls` without an index:
    /// callee as written (`client.send`, `send`) → qualified name.
    pub resolves: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExampleFields {
    code: String,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    resolves: BTreeMap<String, String>,
}

impl<'de> Deserialize<'de> for ExampleSpec {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ExampleVisitor;
        impl<'de> Visitor<'de> for ExampleVisitor {
            type Value = ExampleSpec;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an example: a code string, or a mapping with `code` and optional `language`, `file`, `resolves`")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<ExampleSpec, E> {
                Ok(ExampleSpec {
                    code: v.to_string(),
                    language: None,
                    file: None,
                    resolves: BTreeMap::new(),
                })
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<ExampleSpec, A::Error> {
                let fields = ExampleFields::deserialize(MapAccessDeserializer::new(map))?;
                Ok(ExampleSpec {
                    code: fields.code,
                    language: fields.language,
                    file: fields.file,
                    resolves: fields.resolves,
                })
            }
        }
        deserializer.deserialize_any(ExampleVisitor)
    }
}

/// One value or a list of them. Deserialized by shape (not `untagged`), so
/// an error inside keeps its own message and location.
#[derive(Debug, Clone)]
pub struct OneOrMany<T>(pub Vec<T>);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for OneOrMany<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct OneOrManyVisitor<T>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for OneOrManyVisitor<T> {
            type Value = OneOrMany<T>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("one value or a list")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                Vec::<T>::deserialize(SeqAccessDeserializer::new(seq)).map(OneOrMany)
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                T::deserialize(MapAccessDeserializer::new(map)).map(|one| OneOrMany(vec![one]))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                T::deserialize(v.into_deserializer()).map(|one| OneOrMany(vec![one]))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
                self.visit_str(&v)
            }
        }
        deserializer.deserialize_any(OneOrManyVisitor(PhantomData))
    }
}

/// A YAML document: one rule or a list of rules.
struct RuleDoc(Vec<RuleSpec>);

impl<'de> Deserialize<'de> for RuleDoc {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct DocVisitor;
        impl<'de> Visitor<'de> for DocVisitor {
            type Value = RuleDoc;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a rule (a mapping with `id` and `check-patterns`) or a list of rules")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<RuleDoc, A::Error> {
                Vec::<RuleSpec>::deserialize(SeqAccessDeserializer::new(seq)).map(RuleDoc)
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<RuleDoc, A::Error> {
                RuleSpec::deserialize(MapAccessDeserializer::new(map))
                    .map(|rule| RuleDoc(vec![rule]))
            }
            fn visit_unit<E: de::Error>(self) -> Result<RuleDoc, E> {
                Ok(RuleDoc(Vec::new()))
            }
        }
        deserializer.deserialize_any(DocVisitor)
    }
}

/// Every rule in `text` (all its documents), and each document's YAML
/// error as `line:column: message`. A document that fails does not stop
/// the others, unless the YAML itself is broken.
pub fn parse_rules(text: &str) -> (Vec<RuleSpec>, Vec<String>) {
    let mut rules = Vec::new();
    let mut errors = Vec::new();
    for document in serde_yaml_ng::Deserializer::from_str(text) {
        match RuleDoc::deserialize(document) {
            Ok(doc) => rules.extend(doc.0),
            Err(error) => {
                let broken = error.to_string().contains("while parsing")
                    || error.to_string().contains("while scanning");
                errors.push(yaml_error(&error));
                if broken {
                    break;
                }
            }
        }
    }
    (rules, errors)
}

/// A YAML error as `line:column: message`, with the location moved to the
/// front (serde_yaml_ng appends it) and, for a misspelt key, the key meant.
fn yaml_error(error: &serde_yaml_ng::Error) -> String {
    let mut message = error.to_string();
    if let Some(location) = error.location() {
        let suffix = format!(" at line {} column {}", location.line(), location.column());
        message = message
            .strip_suffix(&suffix)
            .map(str::to_string)
            .unwrap_or_else(|| message.replace(&suffix, ""));
    }
    if let Some(hint) = unknown_field_hint(&message) {
        // The spaced aliases (weggli-ruleset's `check patterns`) only
        // lengthen the list.
        for alias in [
            ", `check pattern`",
            ", `check patterns`",
            ", `ignore pattern`",
            ", `ignore patterns`",
        ] {
            message = message.replace(alias, "");
        }
        message.push_str(&hint);
    }
    match error.location() {
        Some(location) => format!("{}:{}: {message}", location.line(), location.column()),
        None => message,
    }
}

/// ` — did you mean `x`?` for serde's "unknown field `a`, expected one of
/// `b`, `c`".
fn unknown_field_hint(message: &str) -> Option<String> {
    // serde_yaml_ng puts the path first (`check-patterns[0]: unknown …`).
    let rest = message.split_once("unknown field `")?.1;
    let (field, rest) = rest.split_once('`')?;
    let expected: Vec<&str> = rest.split('`').skip(1).step_by(2).collect();
    let normalized = |s: &str| s.to_ascii_lowercase().replace(['_', ' '], "-");
    let wanted = normalized(field);
    expected
        .iter()
        .map(|candidate| (distance(&wanted, &normalized(candidate)), *candidate))
        .filter(|(d, _)| *d <= 3)
        .min()
        .map(|(_, candidate)| format!(" — did you mean `{candidate}`?"))
}

fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut current = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            current[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(current[j] + 1);
        }
        prev = current;
    }
    prev[b.len()]
}

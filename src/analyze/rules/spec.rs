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
        default,
        alias = "check patterns",
        alias = "check-pattern",
        alias = "check pattern"
    )]
    pub check_patterns: Option<OneOrMany<PatternSpec>>,
    /// A taint rule (instead of `check-patterns`): a finding is a sink
    /// whose value a source's value reaches, in one function.
    #[serde(default)]
    pub taint: Option<TaintSpec>,
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
    /// Library models of these kinds instead of a pattern: calls CodeQL's
    /// Models-as-Data (imported, [`super::models`]) model as a sink of
    /// `sql-injection`, a `remote` source… (see `codegraph models list`).
    #[serde(default)]
    pub model: Option<OneOrMany<String>>,
    /// The role a `model` pattern plays (set by the taint role it is in;
    /// a check pattern's models are sinks).
    #[serde(skip)]
    pub model_role: Option<super::models::Role>,
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

/// The roles of a taint rule, each a list of patterns.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct TaintSpec {
    /// Where tainted values come from: `value` names the capture whose
    /// value is tainted (a call's result, a parameter, or storage a call
    /// fills, like C `fgets(buf, …)`'s `buf`).
    pub sources: OneOrMany<TaintPatternSpec>,
    /// Where they must not go: `argument` names the capture whose value
    /// must not be tainted.
    pub sinks: OneOrMany<TaintPatternSpec>,
    /// What makes a value clean: `value` names the capture whose value
    /// (a call's result, or storage a call rewrites in place) is clean.
    #[serde(default)]
    pub sanitizers: Option<OneOrMany<TaintPatternSpec>>,
    /// Extra steps: the data of capture `from` flows into capture `to`.
    #[serde(default)]
    pub propagators: Option<OneOrMany<TaintPatternSpec>>,
    /// Validation guards: `check` names the condition, `value` the value it
    /// checks; reads of the value on the safe branch (`safe: when-true`,
    /// the default, or `when-false`) that the branch dominates are clean.
    #[serde(default)]
    pub guards: Option<OneOrMany<TaintPatternSpec>>,
}

/// A pattern of a taint rule: a weggli `pattern` or a tree-sitter `query`
/// with `regex`/`where` constraints (as a check pattern), plus the
/// capture(s) its role reads.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct TaintPatternSpec {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub language: Option<OneOrMany<String>>,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub query: Option<String>,
    /// Library models of these kinds (sources: threat models like
    /// `remote`, `local`, `environment`; sinks, sanitizers and guards:
    /// vulnerability kinds like `sql-injection` or aliases like `sql`).
    /// The role's capture names default to `model` (the marked value) and,
    /// for guards, `call` (the check).
    #[serde(default)]
    pub model: Option<OneOrMany<String>>,
    #[serde(default, alias = "regexes")]
    pub regex: Option<OneOrMany<String>>,
    #[serde(default, rename = "where")]
    pub where_: Vec<PredicateSpec>,
    /// Finding text for a sink (else the rule's `message`).
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub unique: bool,
    /// Sources and sanitizers: the capture whose value they mark.
    #[serde(default)]
    pub value: Option<String>,
    /// Sinks: the capture whose value must not be tainted.
    #[serde(default)]
    pub argument: Option<String>,
    /// Propagators: the capture whose data flows…
    #[serde(default)]
    pub from: Option<String>,
    /// …into this capture's value.
    #[serde(default)]
    pub to: Option<String>,
    /// Guards: the capture holding the condition.
    #[serde(default)]
    pub check: Option<String>,
    /// Guards: `when-true` (default) or `when-false` — the branch where the
    /// value is safe.
    #[serde(default)]
    pub safe: Option<String>,
}

impl TaintPatternSpec {
    /// The pattern part, as a check pattern (the compiler reports it at
    /// its role's capture once it checked that capture exists).
    pub fn pattern(&self) -> PatternSpec {
        PatternSpec {
            name: self.name.clone(),
            language: self.language.clone(),
            pattern: self.pattern.clone(),
            query: self.query.clone(),
            model: self.model.clone(),
            model_role: None,
            regex: self.regex.clone(),
            where_: self.where_.clone(),
            message: self.message.clone(),
            at: None,
            limit: false,
            unique: self.unique,
        }
    }
}

/// One `where` entry. Exactly one test per entry: a capture test
/// (`capture` + one of `regex`, `not-regex`, `resolves-to`,
/// `not-resolves-to`), `enclosing-function`, `inside`/`not-inside`
/// (optionally with `capture`), or `reached-from` (optionally with
/// `unreached-confidence`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct PredicateSpec {
    /// The function the match is in is reached, through the index's
    /// resolved calls, from an entry point of one of these kinds: `route`,
    /// `extractor`, `listener`, `message`, `public-api`, or `server` (the
    /// first four).
    #[serde(default)]
    pub reached_from: Option<OneOrMany<String>>,
    /// With `reached-from`: a method (or associated fn) also counts as
    /// reached when another method of its type is (a constructor whose
    /// object serves requests).
    #[serde(default)]
    pub via_type: Option<bool>,
    /// With `reached-from`: keep a match it does not reach, ranked at this
    /// confidence (0..=1), instead of dropping it.
    #[serde(default)]
    pub unreached_confidence: Option<f64>,
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
#[derive(Debug, Clone, Deserialize)]
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
    /// How the example's code is reached, for `reached-from` without an
    /// index: `true` (from every kind of entry point), `false` (the
    /// default: from none), or the kinds (`route`, `[listener, message]`).
    pub reached: Option<ReachedSpec>,
}

/// An example's `reached`: a flag or the entry kinds.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ReachedSpec {
    Flag(bool),
    Kinds(OneOrMany<String>),
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
    #[serde(default)]
    reached: Option<ReachedSpec>,
}

impl<'de> Deserialize<'de> for ExampleSpec {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ExampleVisitor;
        impl<'de> Visitor<'de> for ExampleVisitor {
            type Value = ExampleSpec;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an example: a code string, or a mapping with `code` and optional `language`, `file`, `resolves`, `reached`")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<ExampleSpec, E> {
                Ok(ExampleSpec {
                    code: v.to_string(),
                    language: None,
                    file: None,
                    resolves: BTreeMap::new(),
                    reached: None,
                })
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<ExampleSpec, A::Error> {
                let fields = ExampleFields::deserialize(MapAccessDeserializer::new(map))?;
                Ok(ExampleSpec {
                    code: fields.code,
                    language: fields.language,
                    file: fields.file,
                    resolves: fields.resolves,
                    reached: fields.reached,
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
                f.write_str(
                    "a rule (a mapping with `id` and `check-patterns` or `taint`) or a list of \
                     rules",
                )
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

const NO_ANCHOR: &str = "an alias (`*name`) has no anchor (`&name`) before it in the same \
                         document — anchors do not carry across `---`";

/// Every rule in `text` (all its documents), and each document's YAML
/// error as `line:column: message`. A document that fails does not stop
/// the others, unless the YAML itself is broken (or an alias has no
/// anchor, which leaves the stream mid-document).
pub fn parse_rules(text: &str) -> (Vec<RuleSpec>, Vec<String>) {
    let mut rules = Vec::new();
    let mut errors = Vec::new();
    for (index, document) in serde_yaml_ng::Deserializer::from_str(text).enumerate() {
        // An alias with no anchor makes serde_yaml_ng (and its serde_yaml
        // siblings) stop loading mid-document: that document reports
        // "unknown anchor", and the stream then yields the rest as a bogus
        // document whose deserializer `panic!`s on a truncated event list (a
        // lone document can panic at once). Rule text is model-written, so
        // both are one error and the end of the stream, never a crash.
        let parsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            RuleDoc::deserialize(document)
        }));
        let error = match parsed {
            Ok(Ok(doc)) => {
                rules.extend(doc.0);
                continue;
            }
            Ok(Err(error)) => error,
            Err(_) => {
                if errors.is_empty() || index == 0 {
                    errors.push(format!("document {}: {NO_ANCHOR}", index + 1));
                }
                break;
            }
        };
        let text = error.to_string();
        if text.starts_with("unknown anchor") {
            let at = error
                .location()
                .map(|l| format!("{}:{}: ", l.line(), l.column()))
                .unwrap_or_default();
            errors.push(format!("{at}{NO_ANCHOR}"));
            break;
        }
        let message = yaml_error(&error);
        // A syntax error the parser cannot get past repeats for every later
        // "document" (forever, on some inputs): report it once and stop.
        let repeated = errors.last() == Some(&message);
        if !repeated {
            errors.push(message);
        }
        if repeated || text.contains("while parsing") || text.contains("while scanning") {
            break;
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

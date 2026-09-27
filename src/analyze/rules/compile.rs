//! Rules as they run: each YAML rule checked and compiled once — weggli
//! patterns to query trees and tree-sitter queries per language, regexes,
//! predicates — with every error tied to a file, line, rule and pattern.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::sync::LazyLock;

use regex::Regex;
use tree_sitter::Query;

use super::lang::parse_language;
use super::locate::{KeyAt, Locator};
use super::spec::{
    self,
    ExampleSpec,
    OneOrMany,
    PatternSpec,
    PredicateSpec,
    RuleSpec,
    Severity,
    TaintPatternSpec,
    TaintSpec,
};
use super::weggli::query::QueryTree;
use super::weggli::{self, RegexMap};
use crate::extraction::grammar_language;
use crate::types::Language;

/// A problem with a rule file, where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadError {
    /// The file (or `<stdin>`, `builtin:<name>`).
    pub source: String,
    /// 1-based line in it, when known.
    pub line: Option<usize>,
    /// The rule it concerns, when known.
    pub rule: Option<String>,
    pub message: String,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(f, "{}:{line}: ", self.source)?,
            None => write!(f, "{}: ", self.source)?,
        }
        if let Some(rule) = &self.rule {
            write!(f, "rule `{rule}`: ")?;
        }
        f.write_str(&self.message)
    }
}

/// A compiled rule.
pub struct Rule {
    pub id: String,
    pub author: Option<String>,
    pub description: Option<String>,
    pub severity: Severity,
    pub confidence: f64,
    pub tags: Vec<String>,
    pub message: Option<String>,
    pub review: Vec<String>,
    /// The check patterns, or a taint rule's sinks.
    pub checks: Vec<Pattern>,
    pub ignores: Vec<Pattern>,
    /// A taint rule's roles (its sinks are `checks`).
    pub taint: Option<TaintRule>,
    pub examples: Vec<Example>,
    /// Where it was read from, and the line of its `id:`.
    pub source: String,
    pub line: usize,
}

impl Rule {
    /// Whether any pattern runs on `language`.
    pub fn runs_on(&self, language: Language) -> bool {
        self.checks
            .iter()
            .any(|pattern| pattern.backend(language).is_some())
    }

    /// Every pattern of the rule: checks (sinks), ignores, taint roles.
    pub fn patterns(&self) -> impl Iterator<Item = &Pattern> {
        let taint = self.taint.iter().flat_map(|taint| {
            taint
                .sources
                .iter()
                .chain(&taint.sanitizers)
                .map(|role| &role.pattern)
                .chain(taint.propagators.iter().map(|p| &p.pattern))
                .chain(taint.guards.iter().map(|g| &g.pattern))
        });
        self.checks.iter().chain(&self.ignores).chain(taint)
    }

    /// Whether a predicate reads the index (resolution, enclosing calls).
    pub fn uses_index(&self) -> bool {
        self.patterns().any(|pattern| {
            pattern.predicates.iter().any(|predicate| {
                matches!(
                    predicate.kind,
                    PredicateKind::Resolves { .. }
                        | PredicateKind::Enclosing { calls: Some(_), .. }
                        | PredicateKind::Enclosing {
                            calls_not: Some(_),
                            ..
                        }
                )
            })
        })
    }
}

/// A compiled taint rule. Its sinks are the rule's `checks`.
pub struct TaintRule {
    /// Per sink (parallel to `Rule::checks`): the capture whose value must
    /// not be tainted.
    pub sink_values: Vec<String>,
    pub sources: Vec<RolePattern>,
    pub sanitizers: Vec<RolePattern>,
    pub propagators: Vec<PropagatorPattern>,
    pub guards: Vec<GuardPattern>,
}

/// A validation guard: the condition `check` tests the value `value`; the
/// branch where the check holds (`safe_when_true`) or fails is safe.
pub struct GuardPattern {
    pub pattern: Pattern,
    pub value: String,
    pub check: String,
    pub safe_when_true: bool,
}

/// A source or sanitizer: a pattern and the capture whose value it marks.
pub struct RolePattern {
    pub pattern: Pattern,
    pub value: String,
}

/// A propagator: the data of capture `from` flows into capture `to`.
pub struct PropagatorPattern {
    pub pattern: Pattern,
    pub from: String,
    pub to: String,
}

/// A compiled check or ignore pattern.
pub struct Pattern {
    pub name: String,
    /// `check-patterns[0] \`name\`` — how errors and traces name it.
    pub label: String,
    pub line: usize,
    pub backends: Vec<(Language, Backend)>,
    /// `regex` constraints of a `query` pattern (weggli compiles them into
    /// its query tree).
    pub constraints: Vec<Constraint>,
    pub predicates: Vec<Predicate>,
    /// Capture names `where`, `at` and `message` may use (weggli variables
    /// without `$`).
    pub captures: BTreeSet<String>,
    pub message: Option<String>,
    pub at: Option<String>,
    pub limit: bool,
    pub unique: bool,
    /// Every weggli identifier the pattern needs in the file's text.
    pub identifiers: Vec<String>,
}

impl Pattern {
    pub fn backend(&self, language: Language) -> Option<&Backend> {
        self.backends
            .iter()
            .find(|(lang, _)| *lang == language)
            .map(|(_, backend)| backend)
    }
}

pub enum Backend {
    Weggli(Box<QueryTree>),
    Query(Query),
}

pub struct Constraint {
    pub capture: String,
    pub negative: bool,
    pub regex: Regex,
}

pub struct Predicate {
    /// `where[2] (resolves-to: ^x$)` — for traces.
    pub label: String,
    pub kind: PredicateKind,
}

pub enum PredicateKind {
    /// The capture's code matches (or, negative, does not).
    Text {
        capture: String,
        regex: Regex,
        negative: bool,
    },
    /// The call at the capture resolves to a name matching (or not).
    Resolves {
        capture: String,
        regex: Regex,
        negative: bool,
    },
    /// Tests on the enclosing function.
    Enclosing {
        calls: Option<Regex>,
        calls_not: Option<Regex>,
        name: Option<Regex>,
        is_test: Option<bool>,
    },
    /// Some ancestor of the capture (or of the reported node) matches the
    /// query (or, negative, none does).
    Inside {
        capture: Option<String>,
        negative: bool,
        queries: Vec<(Language, Query)>,
    },
}

/// A `bad` or `good` example of a rule.
pub struct Example {
    pub bad: bool,
    pub index: usize,
    pub code: String,
    pub language: Language,
    pub file: String,
    pub resolves: BTreeMap<String, String>,
    pub line: Option<usize>,
}

impl Example {
    pub fn label(&self) -> String {
        format!("{}[{}]", if self.bad { "bad" } else { "good" }, self.index)
    }
}

/// Rules read from one or more sources.
#[derive(Default)]
pub struct RuleSet {
    pub rules: Vec<Rule>,
    /// Rules that did not compile (and whole files that did not parse).
    pub errors: Vec<LoadError>,
}

impl RuleSet {
    /// Read the rules in `text`, reported as `source`, into the set.
    pub fn add_text(&mut self, source: &str, text: &str) {
        let (specs, errors) = spec::parse_rules(text);
        for message in errors {
            let (line, message) = split_location(&message);
            self.errors.push(LoadError {
                source: source.to_string(),
                line,
                rule: None,
                message,
            });
        }
        let locator = Locator::new(text);
        let mut id_lines: Vec<usize> = Vec::new();
        let mut used_lines: HashSet<usize> = HashSet::new();
        for spec in &specs {
            let line = locator
                .rule_lines(&spec.id)
                .into_iter()
                .find(|line| !used_lines.contains(line));
            if let Some(line) = line {
                used_lines.insert(line);
            }
            id_lines.push(line.unwrap_or(0));
        }
        let mut sorted: Vec<usize> = id_lines.iter().copied().filter(|l| *l > 0).collect();
        sorted.sort_unstable();
        let ranges: HashMap<usize, (usize, usize)> = sorted
            .iter()
            .copied()
            .zip(locator.rule_ranges(&sorted))
            .collect();

        for (spec, id_line) in specs.into_iter().zip(id_lines) {
            let range = ranges
                .get(&id_line)
                .copied()
                .unwrap_or((1, locator.line_count() + 1));
            let at = RuleAt {
                source,
                locator: &locator,
                id_line: (id_line > 0).then_some(id_line),
                range,
            };
            let id = spec.id.clone();
            if self.rules.iter().any(|rule| rule.id == id) {
                self.errors.push(at.error(
                    &id,
                    at.id_line,
                    "a rule with this id is already loaded; ids must be unique".to_string(),
                ));
                continue;
            }
            match compile_rule(spec, &at) {
                Ok(rule) => self.rules.push(rule),
                Err(error) => self.errors.push(error),
            }
        }
    }
}

/// `12:3: message` → (Some(12), message).
fn split_location(message: &str) -> (Option<usize>, String) {
    let mut parts = message.splitn(3, ':');
    if let (Some(line), Some(col), Some(rest)) = (parts.next(), parts.next(), parts.next()) {
        if let (Ok(line), Ok(_)) = (line.parse::<usize>(), col.parse::<usize>()) {
            return (Some(line), rest.trim_start().to_string());
        }
    }
    (None, message.to_string())
}

/// Where the rule being compiled sits.
struct RuleAt<'a> {
    source: &'a str,
    locator: &'a Locator<'a>,
    id_line: Option<usize>,
    range: (usize, usize),
}

impl RuleAt<'_> {
    fn error(&self, rule: &str, line: Option<usize>, message: String) -> LoadError {
        LoadError {
            source: self.source.to_string(),
            line: line.or(self.id_line),
            rule: Some(rule.to_string()),
            message,
        }
    }

    fn key(&self, from: usize, to: usize, keys: &[&str]) -> Option<KeyAt> {
        self.locator.key(from, to, keys)
    }

    /// Line range of item `index` of the sequence under `keys` in `from..to`.
    fn item(&self, from: usize, to: usize, keys: &[&str], index: usize) -> Option<(usize, usize)> {
        let key = self.key(from, to, keys)?;
        self.locator.items(key.line, to).get(index).copied()
    }
}

const CHECK_KEYS: &[&str] = &[
    "check-patterns",
    "check patterns",
    "check-pattern",
    "check pattern",
];
/// Languages the IR lowers — where taint rules run.
const TAINT_LANGUAGES: &str = "java, c, cpp, php, python, javascript, typescript";
const IGNORE_KEYS: &[&str] = &[
    "ignore-patterns",
    "ignore patterns",
    "ignore-pattern",
    "ignore pattern",
];

static PLACEHOLDER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{([A-Za-z_][A-Za-z0-9_]*)\}").expect("valid regex"));

/// Placeholders a message may use besides the captures (`source`: a taint
/// finding's source code).
const MESSAGE_BUILTINS: &[&str] = &["function", "source"];

fn compile_rule(spec: RuleSpec, at: &RuleAt) -> Result<Rule, LoadError> {
    let id = spec.id.trim().to_string();
    let err = |line: Option<usize>, message: String| at.error(&id, line, message);
    if id.is_empty() || id.chars().any(char::is_whitespace) {
        return Err(err(
            None,
            "`id` must be a non-empty name without spaces (e.g. `unchecked-malloc`)".into(),
        ));
    }
    if let Some(confidence) = spec.confidence.filter(|c| !(0.0..=1.0).contains(c)) {
        return Err(err(
            at.key(at.range.0, at.range.1, &["confidence"])
                .map(|k| k.line),
            format!("`confidence` must be between 0 and 1, not {confidence}"),
        ));
    }
    let rule_languages = match &spec.language {
        Some(names) => Some(parse_languages(names).map_err(|message| {
            err(
                at.key(at.range.0, at.range.1, &["language"])
                    .map(|k| k.line),
                message,
            )
        })?),
        None => None,
    };

    let (checks, taint) = match (spec.check_patterns, spec.taint) {
        (Some(patterns), None) => (
            compile_checks(patterns, rule_languages.as_deref(), at, &id)?,
            None,
        ),
        (None, Some(taint)) => {
            let (sinks, taint) = compile_taint(taint, rule_languages.as_deref(), at, &id)?;
            (sinks, Some(taint))
        }
        (Some(_), Some(_)) => {
            return Err(err(
                at.key(at.range.0, at.range.1, &["taint"]).map(|k| k.line),
                "has both `check-patterns` and `taint`; a rule is one or the other".into(),
            ));
        }
        (None, None) => {
            return Err(err(
                None,
                "needs `check-patterns` (syntax patterns to match) or `taint` (sources, \
                 sinks, sanitizers: data flow to find)"
                    .into(),
            ));
        }
    };
    let mut ignores = Vec::new();
    for (index, pattern) in spec
        .ignore_patterns
        .map(|p| p.0)
        .unwrap_or_default()
        .into_iter()
        .enumerate()
    {
        let item = at.item(at.range.0, at.range.1, IGNORE_KEYS, index);
        ignores.push(compile_pattern(
            pattern,
            "ignore-patterns",
            index,
            item,
            rule_languages.as_deref(),
            at,
            &id,
        )?);
    }

    // A rule's message is shared by its patterns: each placeholder must be
    // captured by at least one of them (the others show it as written).
    if let Some(message) = &spec.message {
        for caps in PLACEHOLDER.captures_iter(message) {
            let name = &caps[1];
            if !MESSAGE_BUILTINS.contains(&name)
                && !checks.iter().any(|pattern| pattern.captures.contains(name))
            {
                let all: BTreeSet<String> = checks
                    .iter()
                    .flat_map(|pattern| pattern.captures.iter().cloned())
                    .collect();
                return Err(err(
                    at.key(at.range.0, at.range.1, &["message"]).map(|k| k.line),
                    format!(
                        "`message` uses `{{{name}}}`, which no check-pattern captures ({})",
                        available(&all)
                    ),
                ));
            }
        }
    }

    let default_language = checks[0].backends[0].0;
    let examples_key = at.key(at.range.0, at.range.1, &["examples"]);
    let Some(examples) = spec.examples else {
        return Err(err(
            None,
            "no `examples` — every rule needs `examples: {bad: [code it must match], good: \
             [code it must not match]}`; `codegraph analyze rules --check` runs them"
                .into(),
        ));
    };
    if examples.bad.is_empty() {
        return Err(err(
            examples_key.map(|k| k.line),
            "`examples.bad` is empty — give at least one snippet the rule must match".into(),
        ));
    }
    let mut compiled_examples = Vec::new();
    for (bad, list) in [(true, examples.bad), (false, examples.good)] {
        let key = if bad { "bad" } else { "good" };
        let items = examples_key
            .and_then(|k| at.key(k.line, at.range.1, &[key]))
            .map(|k| at.locator.items(k.line, at.range.1))
            .unwrap_or_default();
        for (index, example) in list.into_iter().enumerate() {
            let line = items.get(index).map(|(line, _)| *line);
            compiled_examples.push(
                compile_example(example, bad, index, line, default_language, &checks)
                    .map_err(|message| err(line.or(examples_key.map(|k| k.line)), message))?,
            );
        }
    }

    let severity = spec.severity.unwrap_or_default();
    Ok(Rule {
        confidence: spec.confidence.unwrap_or_else(|| severity.confidence()),
        severity,
        author: spec.author,
        description: spec.description.map(|d| d.trim().to_string()),
        tags: spec.tags,
        message: spec.message,
        review: spec.review,
        checks,
        ignores,
        taint,
        examples: compiled_examples,
        source: at.source.to_string(),
        line: at.id_line.unwrap_or(0),
        id,
    })
}

fn compile_checks(
    patterns: OneOrMany<PatternSpec>,
    rule_languages: Option<&[Language]>,
    at: &RuleAt,
    id: &str,
) -> Result<Vec<Pattern>, LoadError> {
    let mut checks = Vec::new();
    let mut names = HashSet::new();
    for (index, pattern) in patterns.0.into_iter().enumerate() {
        let item = at.item(at.range.0, at.range.1, CHECK_KEYS, index);
        let compiled = compile_pattern(
            pattern,
            "check-patterns",
            index,
            item,
            rule_languages,
            at,
            id,
        )?;
        if !names.insert(compiled.name.clone()) {
            return Err(at.error(
                id,
                item.map(|(line, _)| line),
                format!(
                    "two check-patterns are named `{}`; names must be unique within a rule",
                    compiled.name
                ),
            ));
        }
        checks.push(compiled);
    }
    if checks.is_empty() {
        return Err(at.error(id, None, "`check-patterns` is empty".into()));
    }
    Ok(checks)
}

/// The keys each taint role takes, and says what they mean.
fn role_keys(role: &str) -> (&'static [&'static str], &'static str) {
    match role {
        "sinks" => (
            &["argument"],
            "`argument: <capture>` — the value that must not be tainted",
        ),
        "propagators" => (
            &["from", "to"],
            "`from: <capture>` and `to: <capture>` — the data of `from` flows into `to`",
        ),
        "guards" => (
            &["value", "check"],
            "`value: <capture>` (the checked value) and `check: <capture>` (the condition)",
        ),
        _ => (&["value"], "`value: <capture>` — the value it marks"),
    }
}

/// A taint rule's roles; its sinks become the rule's check patterns.
fn compile_taint(
    spec: TaintSpec,
    rule_languages: Option<&[Language]>,
    at: &RuleAt,
    id: &str,
) -> Result<(Vec<Pattern>, TaintRule), LoadError> {
    let taint_line = at
        .key(at.range.0, at.range.1, &["taint"])
        .map_or(at.range.0, |k| k.line);
    let mut taint = TaintRule {
        sink_values: Vec::new(),
        sources: Vec::new(),
        sanitizers: Vec::new(),
        propagators: Vec::new(),
        guards: Vec::new(),
    };
    let mut sinks = Vec::new();
    let lists = [
        ("sources", spec.sources.0),
        ("sinks", spec.sinks.0),
        (
            "sanitizers",
            spec.sanitizers.map(|p| p.0).unwrap_or_default(),
        ),
        (
            "propagators",
            spec.propagators.map(|p| p.0).unwrap_or_default(),
        ),
        ("guards", spec.guards.map(|p| p.0).unwrap_or_default()),
    ];
    for (role, list) in lists {
        let role_line = at.key(taint_line, at.range.1, &[role]).map(|k| k.line);
        if list.is_empty() && matches!(role, "sources" | "sinks") {
            return Err(at.error(
                id,
                role_line,
                format!("`taint.{role}` is empty — a taint rule needs sources and sinks"),
            ));
        }
        for (index, pattern) in list.into_iter().enumerate() {
            let item = at.item(taint_line, at.range.1, &[role], index);
            let safe_when_true = match pattern.safe.as_deref() {
                None | Some("when-true") => true,
                Some("when-false") => false,
                Some(other) => {
                    return Err(at.error(
                        id,
                        item.map(|(line, _)| line),
                        format!(
                            "taint.{role}[{index}]: `safe: {other}` — a guard is safe \
                             `when-true` (the branch where the check holds) or `when-false`"
                        ),
                    ));
                }
            };
            if pattern.safe.is_some() && role != "guards" {
                return Err(at.error(
                    id,
                    item.map(|(line, _)| line),
                    format!("taint.{role}[{index}]: `safe` is for guards, not {role}"),
                ));
            }
            let (pattern, captures) =
                compile_taint_pattern(pattern, role, index, item, rule_languages, at, id)?;
            match role {
                "sources" => taint.sources.push(RolePattern {
                    pattern,
                    value: captures[0].clone(),
                }),
                "sanitizers" => taint.sanitizers.push(RolePattern {
                    pattern,
                    value: captures[0].clone(),
                }),
                "propagators" => taint.propagators.push(PropagatorPattern {
                    pattern,
                    from: captures[0].clone(),
                    to: captures[1].clone(),
                }),
                "guards" => taint.guards.push(GuardPattern {
                    pattern,
                    value: captures[0].clone(),
                    check: captures[1].clone(),
                    safe_when_true,
                }),
                _ => {
                    taint.sink_values.push(captures[0].clone());
                    sinks.push(pattern);
                }
            }
        }
    }
    Ok((sinks, taint))
}

/// One pattern of a taint role, and the captures its role names (in the
/// order of [`role_keys`]).
fn compile_taint_pattern(
    spec: TaintPatternSpec,
    role: &str,
    index: usize,
    item: Option<(usize, usize)>,
    rule_languages: Option<&[Language]>,
    at: &RuleAt,
    id: &str,
) -> Result<(Pattern, Vec<String>), LoadError> {
    let section = format!("taint.{role}");
    let label = match &spec.name {
        Some(name) => format!("{section}[{index}] `{name}`"),
        None => format!("{section}[{index}]"),
    };
    let (from, to) = item.unwrap_or(at.range);
    let key_line = |keys: &[&str]| at.key(from, to, keys).map(|k| k.line);
    let err = |keys: &[&str], message: String| {
        at.error(
            id,
            key_line(keys).or(item.map(|(line, _)| line)),
            format!("{label}: {message}"),
        )
    };
    let (wanted, usage) = role_keys(role);
    let given = [
        ("value", &spec.value),
        ("argument", &spec.argument),
        ("from", &spec.from),
        ("to", &spec.to),
        ("check", &spec.check),
    ];
    for (key, value) in given {
        if value.is_some() && !wanted.contains(&key) {
            return Err(err(
                &[key],
                format!("`{key}` does not apply to {role}; they take {usage}"),
            ));
        }
    }
    if spec.message.is_some() && role != "sinks" {
        return Err(err(
            &["message"],
            format!("`message` is for sinks (the finding's text), not {role}"),
        ));
    }
    let names: Vec<Option<String>> = wanted
        .iter()
        .map(|key| {
            given
                .iter()
                .find(|(k, _)| k == key)
                .and_then(|(_, value)| (*value).clone())
        })
        .collect();
    if names.iter().any(Option::is_none) {
        return Err(err(&[], format!("{role} need {usage}")));
    }
    let names: Vec<String> = names.into_iter().flatten().collect();
    let mut pattern = compile_pattern(
        spec.pattern(),
        &section,
        index,
        item,
        rule_languages,
        at,
        id,
    )?;
    for (key, name) in wanted.iter().zip(&names) {
        if !pattern.captures.contains(name) {
            return Err(err(
                &[key],
                format!(
                    "`{key}: {name}` names no capture ({})",
                    available(&pattern.captures)
                ),
            ));
        }
    }
    for (language, _) in &pattern.backends {
        if super::lang::for_language(*language).ir.is_none() {
            return Err(err(
                &["language"],
                format!(
                    "taint rules run on the languages the IR lowers ({TAINT_LANGUAGES}), not {}",
                    language.as_str()
                ),
            ));
        }
    }
    pattern.at = Some(names[0].clone());
    Ok((pattern, names))
}

fn parse_languages(names: &OneOrMany<String>) -> Result<Vec<Language>, String> {
    let mut languages = Vec::new();
    for name in &names.0 {
        let language = parse_language(name)?;
        if !languages.contains(&language) {
            languages.push(language);
        }
    }
    if languages.is_empty() {
        return Err("`language` is empty".into());
    }
    Ok(languages)
}

fn example_file(language: Language) -> String {
    let ext = match language {
        Language::C => "c",
        Language::Cpp => "cpp",
        Language::Rust => "rs",
        Language::Python => "py",
        Language::Go => "go",
        Language::Javascript => "js",
        Language::Jsx => "jsx",
        Language::Typescript => "ts",
        Language::Tsx => "tsx",
        Language::Java => "java",
        Language::Csharp => "cs",
        Language::Php => "php",
        Language::Ruby => "rb",
        Language::Kotlin => "kt",
        Language::Swift => "swift",
        other => other.as_str(),
    };
    format!("example.{ext}")
}

fn compile_example(
    example: ExampleSpec,
    bad: bool,
    index: usize,
    line: Option<usize>,
    default_language: Language,
    checks: &[Pattern],
) -> Result<Example, String> {
    let label = format!("examples.{}[{index}]", if bad { "bad" } else { "good" });
    let language = match &example.language {
        Some(name) => parse_language(name).map_err(|e| format!("{label}: {e}"))?,
        None => default_language,
    };
    if !checks
        .iter()
        .any(|pattern| pattern.backend(language).is_some())
    {
        return Err(format!(
            "{label} is {}, which no check-pattern runs on",
            language.as_str()
        ));
    }
    Ok(Example {
        bad,
        index,
        code: example.code,
        language,
        file: example.file.unwrap_or_else(|| example_file(language)),
        resolves: example.resolves,
        line,
    })
}

fn check_placeholders(message: &str, pattern: &Pattern) -> Result<(), String> {
    for caps in PLACEHOLDER.captures_iter(message) {
        let name = &caps[1];
        if !pattern.captures.contains(name) && !MESSAGE_BUILTINS.contains(&name) {
            return Err(format!(
                "`message` uses `{{{name}}}`, which {} does not capture ({})",
                pattern.label,
                available(&pattern.captures)
            ));
        }
    }
    Ok(())
}

fn available(captures: &BTreeSet<String>) -> String {
    if captures.is_empty() {
        "it captures nothing".to_string()
    } else {
        format!(
            "captures: {}",
            captures.iter().cloned().collect::<Vec<_>>().join(", ")
        )
    }
}

fn compile_pattern(
    spec: PatternSpec,
    section: &str,
    index: usize,
    item: Option<(usize, usize)>,
    rule_languages: Option<&[Language]>,
    at: &RuleAt,
    rule: &str,
) -> Result<Pattern, LoadError> {
    let name = spec
        .name
        .clone()
        .unwrap_or_else(|| format!("{section}[{index}]"));
    let label = if spec.name.is_some() {
        format!("{section}[{index}] `{name}`")
    } else {
        format!("{section}[{index}]")
    };
    let (from, to) = item.unwrap_or(at.range);
    let item_line = item.map(|(line, _)| line);
    let err = |line: Option<usize>, message: String| {
        at.error(rule, line.or(item_line), format!("{label}: {message}"))
    };
    let key_line = |keys: &[&str]| at.key(from, to, keys).map(|k| k.line);

    let (text, is_weggli) = match (&spec.pattern, &spec.query) {
        (Some(pattern), None) => (pattern.clone(), true),
        (None, Some(query)) => (query.clone(), false),
        (Some(_), Some(_)) => {
            return Err(err(
                None,
                "has both `pattern` (weggli, C/C++) and `query` (tree-sitter); give one".into(),
            ));
        }
        (None, None) => {
            return Err(err(
                None,
                "needs `pattern` (a weggli pattern, C/C++) or `query` (a tree-sitter query)".into(),
            ));
        }
    };
    let text_key = at.key(from, to, if is_weggli { &["pattern"] } else { &["query"] });

    let languages = match &spec.language {
        Some(names) => parse_languages(names).map_err(|m| err(key_line(&["language"]), m))?,
        None => match rule_languages {
            Some(languages) => languages.to_vec(),
            None if is_weggli => vec![Language::C, Language::Cpp],
            None => {
                return Err(err(
                    None,
                    "a `query` pattern needs `language:` (on the rule or the pattern) — the \
                     grammar it is written against"
                        .into(),
                ));
            }
        },
    };

    // `var=regex` / `var!=regex`.
    let mut regexes: Vec<(String, bool, Regex)> = Vec::new();
    for raw in spec.regex.map(|r| r.0).unwrap_or_default() {
        let line = key_line(&["regex", "regexes"]);
        let Some((var, re)) = raw.split_once('=') else {
            return Err(err(
                line,
                format!("regex `{raw}` is not `var=regex` (or `var!=regex`)"),
            ));
        };
        let mut var = var.trim().trim_start_matches('$').to_string();
        let negative = var.ends_with('!');
        if negative {
            var.pop();
        }
        let regex = Regex::new(re.trim())
            .map_err(|e| err(line, format!("regex for `{var}` does not compile: {e}")))?;
        regexes.push((var, negative, regex));
    }

    let mut backends = Vec::new();
    let mut captures = BTreeSet::new();
    let mut identifiers = Vec::new();
    let mut constraints = Vec::new();
    if is_weggli {
        let map = RegexMap::new(
            regexes
                .iter()
                .map(|(var, negative, regex)| (format!("${var}"), (*negative, regex.clone())))
                .collect(),
        );
        for &language in &languages {
            let cpp = match language {
                Language::C => false,
                Language::Cpp => true,
                other => {
                    return Err(err(
                        key_line(&["language"]),
                        format!(
                            "weggli patterns match C and C++ only, not {}; use `query` (a \
                             tree-sitter query) for other languages",
                            other.as_str()
                        ),
                    ));
                }
            };
            let tree = weggli::parse_search_pattern(&text, cpp, false, Some(map.clone())).map_err(
                |e| {
                    let line = match (e.position, text_key) {
                        (Some((row, _)), Some(key)) => Some(key.value_line(row)),
                        _ => text_key.map(|k| k.line),
                    };
                    err(
                        line,
                        format!(
                            "pattern ({}): {}{}",
                            if cpp { "C++" } else { "C" },
                            e.message,
                            if languages.len() > 1 {
                                " — if it is meant for one language only, set `language` on \
                                 this pattern"
                            } else {
                                ""
                            }
                        ),
                    )
                },
            )?;
            let variables = tree.variables();
            for (var, _, _) in &regexes {
                if !variables.contains(&format!("${var}")) {
                    return Err(err(
                        key_line(&["regex", "regexes"]),
                        format!(
                            "regex constrains `${var}`, which the pattern does not use \
                             (variables: {})",
                            sorted_vars(&variables)
                        ),
                    ));
                }
            }
            captures.extend(
                variables
                    .iter()
                    .map(|var| var.trim_start_matches('$').to_string()),
            );
            if identifiers.is_empty() {
                identifiers = tree.identifiers();
            }
            backends.push((language, Backend::Weggli(Box::new(tree))));
        }
    } else {
        for &language in &languages {
            let query = compile_query(&text, language).map_err(|(row, message)| {
                let line = match (row, text_key) {
                    (Some(row), Some(key)) => Some(key.value_line(row)),
                    _ => text_key.map(|k| k.line),
                };
                err(line, format!("query ({}): {message}", language.as_str()))
            })?;
            captures.extend(query.capture_names().iter().map(|name| name.to_string()));
            backends.push((language, Backend::Query(query)));
        }
        for (var, negative, regex) in regexes {
            if !captures.contains(&var) {
                return Err(err(
                    key_line(&["regex", "regexes"]),
                    format!(
                        "regex constrains `{var}`, which the query does not capture ({})",
                        available(&captures)
                    ),
                ));
            }
            constraints.push(Constraint {
                capture: var,
                negative,
                regex,
            });
        }
    }

    let where_items = at
        .key(from, to, &["where"])
        .map(|k| at.locator.items(k.line, to))
        .unwrap_or_default();
    let mut predicates = Vec::new();
    for (j, predicate) in spec.where_.into_iter().enumerate() {
        let line = where_items.get(j).map(|(line, _)| *line);
        predicates.push(
            compile_predicate(predicate, j, &captures, &languages)
                .map_err(|message| err(line.or(key_line(&["where"])), message))?,
        );
    }

    if let Some(capture) = spec.at.as_ref().filter(|c| !captures.contains(*c)) {
        return Err(err(
            key_line(&["at"]),
            format!(
                "`at: {capture}` names no capture ({})",
                available(&captures)
            ),
        ));
    }

    let pattern = Pattern {
        name,
        label: label.clone(),
        line: item_line.or(text_key.map(|k| k.line)).unwrap_or(0),
        backends,
        constraints,
        predicates,
        captures,
        message: spec.message,
        at: spec.at,
        limit: spec.limit,
        unique: spec.unique,
        identifiers,
    };
    if let Some(message) = &pattern.message {
        check_placeholders(message, &pattern).map_err(|m| err(key_line(&["message"]), m))?;
    }
    Ok(pattern)
}

fn sorted_vars(variables: &HashSet<String>) -> String {
    let mut vars: Vec<&String> = variables.iter().collect();
    vars.sort();
    if vars.is_empty() {
        "none".to_string()
    } else {
        vars.iter()
            .map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Predicates a query may carry that tree-sitter evaluates itself.
const TEXT_PREDICATES: &str = "#eq?, #not-eq?, #any-eq?, #any-not-eq?, #match?, #not-match?, \
                               #any-match?, #any-not-match?, #any-of?, #not-any-of?";

/// A tree-sitter query for `language`, or the (0-based row, message) of
/// what is wrong with it.
pub(super) fn compile_query(
    text: &str,
    language: Language,
) -> Result<Query, (Option<usize>, String)> {
    let grammar = grammar_language(language)
        .ok_or_else(|| (None, format!("no grammar for {}", language.as_str())))?;
    let query = Query::new(&grammar, text).map_err(|e| {
        let name = e.message.trim().trim_matches('"').to_string();
        let what = match e.kind {
            tree_sitter::QueryErrorKind::NodeType => {
                let suggestions = similar(&name, node_kinds(&grammar));
                format!(
                    "`{name}` is not a node kind of the {} grammar{suggestions}",
                    language.as_str()
                )
            }
            tree_sitter::QueryErrorKind::Field => {
                let suggestions = similar(&name, field_names(&grammar));
                format!(
                    "`{name}` is not a field of the {} grammar{suggestions}",
                    language.as_str()
                )
            }
            tree_sitter::QueryErrorKind::Capture => {
                format!(
                    "capture `@{}` is used in a predicate but never defined",
                    e.message
                )
            }
            tree_sitter::QueryErrorKind::Structure => format!(
                "this node cannot appear there in the {} grammar: `{}`",
                language.as_str(),
                e.message.lines().next().unwrap_or_default()
            ),
            kind => format!("{kind:?} error: {}", e.message),
        };
        (
            Some(e.row + 1),
            format!("{what} (query line {}, column {})", e.row + 1, e.column + 1),
        )
    })?;
    for index in 0..query.pattern_count() {
        if let Some(predicate) = query.general_predicates(index).first() {
            return Err((
                None,
                format!(
                    "unsupported predicate `#{}` — queries may use {TEXT_PREDICATES}; put \
                     semantic tests in `where`",
                    predicate.operator
                ),
            ));
        }
    }
    Ok(query)
}

fn node_kinds(grammar: &tree_sitter::Language) -> Vec<String> {
    let mut kinds: Vec<String> = (0..grammar.node_kind_count() as u16)
        .filter(|&id| grammar.node_kind_is_named(id) && grammar.node_kind_is_visible(id))
        .filter_map(|id| grammar.node_kind_for_id(id))
        .map(str::to_string)
        .collect();
    kinds.sort();
    kinds.dedup();
    kinds
}

fn field_names(grammar: &tree_sitter::Language) -> Vec<String> {
    (1..=grammar.field_count() as u16)
        .filter_map(|id| grammar.field_name_for_id(id))
        .map(str::to_string)
        .collect()
}

/// ` — did you mean: a, b, c?` for the names closest to `wrong`.
fn similar(wrong: &str, names: Vec<String>) -> String {
    let wrong = wrong.trim();
    let mut scored: Vec<(usize, String)> = names
        .into_iter()
        .map(|name| {
            let distance = if name.contains(wrong) || wrong.contains(name.as_str()) {
                0
            } else {
                edit_distance(wrong, &name)
            };
            (distance, name)
        })
        .filter(|(distance, name)| *distance <= (wrong.len().max(name.len()) / 3).max(2))
        .collect();
    scored.sort();
    scored.truncate(5);
    if scored.is_empty() {
        String::new()
    } else {
        format!(
            " — did you mean: {}?",
            scored
                .into_iter()
                .map(|(_, name)| name)
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn edit_distance(a: &str, b: &str) -> usize {
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

fn compile_predicate(
    spec: PredicateSpec,
    index: usize,
    captures: &BTreeSet<String>,
    languages: &[Language],
) -> Result<Predicate, String> {
    let prefix = format!("where[{index}]");
    let regex = |key: &str, raw: &str| {
        Regex::new(raw).map_err(|e| format!("{prefix}: `{key}` regex does not compile: {e}"))
    };
    let check_capture = |capture: &str| {
        if captures.contains(capture) {
            Ok(())
        } else {
            Err(format!(
                "{prefix}: `capture: {capture}` names no capture of this pattern ({})",
                available(captures)
            ))
        }
    };

    let capture_tests = [
        ("regex", &spec.regex),
        ("not-regex", &spec.not_regex),
        ("resolves-to", &spec.resolves_to),
        ("not-resolves-to", &spec.not_resolves_to),
    ];
    let given: Vec<(&str, &String)> = capture_tests
        .iter()
        .filter_map(|(key, value)| value.as_ref().map(|v| (*key, v)))
        .collect();
    let inside = [("inside", &spec.inside), ("not-inside", &spec.not_inside)];
    let inside_given: Vec<(&str, &String)> = inside
        .iter()
        .filter_map(|(key, value)| value.as_ref().map(|v| (*key, v)))
        .collect();
    let forms = usize::from(!given.is_empty())
        + usize::from(spec.enclosing_function.is_some())
        + usize::from(!inside_given.is_empty());
    let usage = "each `where` entry is one of: `{capture: X, regex|not-regex|resolves-to|\
                 not-resolves-to: RE}`, `{enclosing-function: {calls|calls-not|name-regex: RE, \
                 is-test: BOOL}}`, `{inside|not-inside: QUERY, capture: X (optional)}`";
    if forms != 1 || given.len() > 1 || inside_given.len() > 1 {
        return Err(format!("{prefix}: {usage}"));
    }

    if let Some(enclosing) = spec.enclosing_function {
        if spec.capture.is_some() {
            return Err(format!(
                "{prefix}: `enclosing-function` takes no `capture` (it tests the function the \
                 match is in)"
            ));
        }
        if enclosing.calls.is_none()
            && enclosing.calls_not.is_none()
            && enclosing.name_regex.is_none()
            && enclosing.is_test.is_none()
        {
            return Err(format!(
                "{prefix}: `enclosing-function` needs at least one of calls, calls-not, \
                 name-regex, is-test"
            ));
        }
        let mut parts = Vec::new();
        for (key, value) in [
            ("calls", &enclosing.calls),
            ("calls-not", &enclosing.calls_not),
            ("name-regex", &enclosing.name_regex),
        ] {
            if let Some(value) = value {
                parts.push(format!("{key}: {value}"));
            }
        }
        if let Some(is_test) = enclosing.is_test {
            parts.push(format!("is-test: {is_test}"));
        }
        return Ok(Predicate {
            label: format!("{prefix} (enclosing-function {})", parts.join(", ")),
            kind: PredicateKind::Enclosing {
                calls: enclosing
                    .calls
                    .as_deref()
                    .map(|r| regex("calls", r))
                    .transpose()?,
                calls_not: enclosing
                    .calls_not
                    .as_deref()
                    .map(|r| regex("calls-not", r))
                    .transpose()?,
                name: enclosing
                    .name_regex
                    .as_deref()
                    .map(|r| regex("name-regex", r))
                    .transpose()?,
                is_test: enclosing.is_test,
            },
        });
    }

    if let Some((key, query)) = inside_given.first() {
        if let Some(capture) = &spec.capture {
            check_capture(capture)?;
        }
        let mut queries = Vec::new();
        for &language in languages {
            let compiled = compile_query(query, language).map_err(|(_, message)| {
                format!("{prefix}: `{key}` query ({}): {message}", language.as_str())
            })?;
            queries.push((language, compiled));
        }
        return Ok(Predicate {
            label: format!("{prefix} ({key}: {})", query.trim()),
            kind: PredicateKind::Inside {
                capture: spec.capture.clone(),
                negative: *key == "not-inside",
                queries,
            },
        });
    }

    let (key, raw) = given[0];
    let Some(capture) = spec.capture else {
        return Err(format!(
            "{prefix}: `{key}` needs `capture:` — the capture it tests"
        ));
    };
    check_capture(&capture)?;
    let compiled = regex(key, raw)?;
    let label = format!("{prefix} ({key}: {raw} on `{capture}`)");
    let kind = match key {
        "regex" | "not-regex" => PredicateKind::Text {
            capture,
            regex: compiled,
            negative: key == "not-regex",
        },
        _ => PredicateKind::Resolves {
            capture,
            regex: compiled,
            negative: key == "not-resolves-to",
        },
    };
    Ok(Predicate { label, kind })
}

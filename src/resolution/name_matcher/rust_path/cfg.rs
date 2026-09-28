//! `#[cfg(…)]` alternatives, decided where a build cannot have both.
//!
//! Tree-sitter reads every `cfg` branch, so a name can bind twice: tokio's
//! `#[cfg(not(all(test, loom)))] pub(crate) use self::std::*;` beside
//! `#[cfg(all(test, loom))] pub(crate) use self::mocked::*;` (its loom
//! facade), a `#[cfg(windows)]` fn beside its `#[cfg(unix)]` twin. Only
//! one of them is compiled. A `cfg` nobody passes to rustc (`loom`,
//! `miri`, `docsrs`, `tokio_unstable`) is off, and target predicates
//! answer for the host the index is built on (64-bit little-endian
//! linux); those decide first. `test` never decides: `cargo test` and
//! `cargo build` are both real builds (rust-analyzer, the compiler layer's
//! judge, analyses with `cfg(test)` on), so a test twin stays a tie, picked
//! by nearness. A `feature` decides only what is still tied after that,
//! and only by the item's own attributes: the crate's default features
//! (`default = […]`, expanded) — tokio's `parking_lot` `Mutex` beside
//! the std one is the std one ([`BuildCfg`]). Whole modules are never
//! ruled out by a feature: a library's optional features gate most of it.
//! A `cfg` decides only between alternatives: a name bound once keeps its
//! one binding whatever guards it.
//!
//! Attributes are read from the source once per file ([`file_cfgs`]), a
//! manifest's default features once per manifest.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::layout::{ModuleLocation, item_location, module_location};
use crate::resolution::line_index::Lines;
use crate::resolution::types::ResolutionContext;
use crate::types::{Language, Node, NodeKind};

/// How deep a file's module chain is followed for `#[cfg] mod m;`.
const MAX_MODULE_DEPTH: usize = 12;
/// How many lines an attribute may span.
const MAX_ATTRIBUTE_LINES: usize = 12;

/// Whether an item exists in the default build: `Some(false)` when a
/// `cfg` rules it out, `Some(true)` when one admits it, `None` without one.
pub(in crate::resolution::name_matcher) type Active = Option<bool>;

/// The `cfg` predicates guarding each item of a file, by the 1-based line
/// the item starts on (after its attributes).
#[derive(Debug, Default)]
struct FileCfgs {
    by_line: HashMap<u32, Vec<String>>,
}

/// What a build is known to set, beyond the host target.
#[derive(Clone, Copy)]
enum Features<'a> {
    /// Any feature may be on: `feature = "…"` is unknown.
    Unknown,
    /// Exactly these (a crate's default features).
    Default(&'a HashSet<String>),
}

/// Whether the item starting on `line` of `file` (a `use`, a fn, a `mod`
/// declaration) is compiled on the host, by its own attributes, whatever
/// the features.
pub(in crate::resolution::name_matcher) fn line_active(
    context: &dyn ResolutionContext,
    file: &str,
    line: u32,
) -> Active {
    let cfgs = file_cfgs(context, file);
    let predicates = cfgs.by_line.get(&line)?;
    all_of(
        predicates
            .iter()
            .map(|predicate| evaluate(predicate, Features::Unknown)),
    )
}

/// Whether the item starting on `line` of `file` is compiled with its
/// crate's default features, by its own attributes.
pub(in crate::resolution::name_matcher) fn line_in_default_build(
    context: &dyn ResolutionContext,
    file: &str,
    line: u32,
) -> Active {
    let cfgs = file_cfgs(context, file);
    let predicates = cfgs.by_line.get(&line)?;
    let features = default_features(context, file);
    all_of(
        predicates
            .iter()
            .map(|predicate| evaluate(predicate, Features::Default(&features))),
    )
}

/// Whether an alternative is compiled: on the host (`host`, any
/// features), then with its crate's default features (`default`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::resolution::name_matcher) struct BuildCfg {
    pub(in crate::resolution::name_matcher) host: Active,
    pub(in crate::resolution::name_matcher) default: Active,
}

impl BuildCfg {
    /// What a binding with no attributes of its own says.
    pub(in crate::resolution::name_matcher) const UNKNOWN: BuildCfg = BuildCfg {
        host: None,
        default: None,
    };

    /// An item's: its own attributes and its module chain on the host,
    /// its own attributes with default features.
    pub(in crate::resolution::name_matcher) fn of_node(
        context: &dyn ResolutionContext,
        node: &Node,
    ) -> BuildCfg {
        BuildCfg {
            host: node_active(context, node),
            default: line_in_default_build(context, &node.file_path, node.start_line),
        }
    }

    /// A `use` declaration's, starting on `line` of `file`.
    pub(in crate::resolution::name_matcher) fn of_line(
        context: &dyn ResolutionContext,
        file: &str,
        line: u32,
    ) -> BuildCfg {
        BuildCfg {
            host: line_active(context, file, line),
            default: line_in_default_build(context, file, line),
        }
    }
}

/// Of several alternatives, those the host compiles; of those still
/// tied, the ones the default features compile — all of them when a tier
/// rules every one out or none.
pub(in crate::resolution::name_matcher) fn keep_built<T>(
    alternatives: Vec<T>,
    build: impl Fn(&T) -> BuildCfg,
) -> Vec<T> {
    let kept = keep_active(alternatives, |alternative| build(alternative).host);
    keep_active(kept, |alternative| build(alternative).default)
}

/// Whether `node` exists in the default build: its own attributes, then
/// every `mod` declaration its file hangs from (`#[cfg(test)] mod mocks;`
/// rules out all of `mocks.rs`).
pub(in crate::resolution::name_matcher) fn node_active(
    context: &dyn ResolutionContext,
    node: &Node,
) -> Active {
    if node.language != Language::Rust {
        return None;
    }
    let own = line_active(context, &node.file_path, node.start_line);
    if own == Some(false) {
        return own;
    }
    // Inline modules around the item, then the file's own module chain.
    let (location, _) = item_location(node);
    both(own, module_active(context, &location, 0))
}

/// Whether the module at `location` exists in the default build, by the
/// `mod` declarations leading to it.
fn module_active(
    context: &dyn ResolutionContext,
    location: &ModuleLocation,
    depth: usize,
) -> Active {
    if depth > MAX_MODULE_DEPTH {
        return None;
    }
    let parent = location.parent()?;
    let name = location.module.last()?;
    let declared = context
        .get_nodes_by_name_and_kind(name, NodeKind::Module)
        .into_iter()
        .find(|node| node.language == Language::Rust && item_location(node).0 == parent);
    let own = match declared {
        Some(node) => line_active(context, &node.file_path, node.start_line),
        // `mod m;` inside a macro call: no Module node, its line unknown.
        None => None,
    };
    if own == Some(false) {
        return own;
    }
    both(own, module_active(context, &parent, depth + 1))
}

/// Of several alternatives, the ones the default build has — all of them
/// when it has none or cannot tell.
pub(in crate::resolution::name_matcher) fn keep_active<T>(
    alternatives: Vec<T>,
    active: impl Fn(&T) -> Active,
) -> Vec<T> {
    if alternatives.len() < 2 {
        return alternatives;
    }
    let verdicts: Vec<Active> = alternatives.iter().map(&active).collect();
    if !verdicts.contains(&Some(false)) || verdicts.iter().all(|verdict| *verdict == Some(false)) {
        return alternatives;
    }
    alternatives
        .into_iter()
        .zip(verdicts)
        .filter(|(_, verdict)| *verdict != Some(false))
        .map(|(alternative, _)| alternative)
        .collect()
}

/// Three-valued `all(…)`: false wins, else an unknown leaves it unknown.
fn all_of(values: impl Iterator<Item = Active>) -> Active {
    let mut all = Some(true);
    for value in values {
        all = and(all, value);
    }
    all
}

/// Three-valued `any(…)`: true wins, else an unknown leaves it unknown.
fn any_of(values: impl Iterator<Item = Active>) -> Active {
    let mut any = Some(false);
    for value in values {
        any = match (any, value) {
            (Some(true), _) | (_, Some(true)) => Some(true),
            (Some(false), Some(false)) => Some(false),
            _ => None,
        };
    }
    any
}

/// Three-valued `all`: false wins, an unknown leaves it unknown.
fn and(a: Active, b: Active) -> Active {
    match (a, b) {
        (Some(false), _) | (_, Some(false)) => Some(false),
        (Some(true), Some(true)) => Some(true),
        _ => None,
    }
}

/// Two facts about one item (its own `cfg`, its module's), where `None`
/// is no `cfg` at all: ruled out when either rules it out.
fn both(a: Active, b: Active) -> Active {
    match (a, b) {
        (Some(false), _) | (_, Some(false)) => Some(false),
        _ => a.or(b),
    }
}

fn file_cfgs(context: &dyn ResolutionContext, file: &str) -> Arc<FileCfgs> {
    let derived = context.get_rust_file_derived(file, "rust-cfg-attributes", &mut || {
        let cfgs = context
            .read_file_arc(file)
            .map(|source| scan(&source))
            .unwrap_or_default();
        Arc::new(cfgs)
    });
    derived.downcast::<FileCfgs>().unwrap_or_default()
}

/// Each item's `#[cfg(…)]` attributes: the attribute lines above the line
/// the item starts on (doc comments and other attributes between them).
fn scan(source: &Arc<str>) -> FileCfgs {
    let lines = Lines::of(source);
    let lines = lines.span();
    let mut by_line: HashMap<u32, Vec<String>> = HashMap::new();
    let mut pending: Vec<String> = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let Some(line) = lines.get(index) else {
            break;
        };
        let text = line.trim();
        if text.starts_with("#[") {
            // An attribute, possibly over several lines.
            let mut attribute = text.to_string();
            let mut end = index;
            while !balanced(&attribute)
                && end + 1 < lines.len()
                && end - index < MAX_ATTRIBUTE_LINES
            {
                end += 1;
                attribute.push(' ');
                attribute.push_str(lines.get(end).unwrap_or_default().trim());
            }
            if let Some(predicate) = cfg_predicate(&attribute) {
                pending.push(predicate);
            }
            index = end + 1;
            continue;
        }
        if text.is_empty() || text.starts_with("//") {
            index += 1;
            continue;
        }
        if !pending.is_empty() {
            let line_number = u32::try_from(index + 1).unwrap_or(u32::MAX);
            by_line.insert(line_number, std::mem::take(&mut pending));
        }
        index += 1;
    }
    FileCfgs { by_line }
}

/// The brackets of an attribute's text are closed.
fn balanced(text: &str) -> bool {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for byte in text.bytes() {
        if in_string {
            match byte {
                b'\\' if !escaped => escaped = true,
                b'"' if !escaped => in_string = false,
                _ => escaped = false,
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'[' | b'(' => depth += 1,
            b']' | b')' => depth -= 1,
            _ => {}
        }
    }
    depth <= 0
}

/// `#[cfg(pred)]` -> `pred`; any other attribute (`cfg_attr` included) ->
/// `None`.
fn cfg_predicate(attribute: &str) -> Option<String> {
    let inner = attribute.strip_prefix("#[")?.trim_start();
    let rest = inner.strip_prefix("cfg")?.trim_start();
    let rest = rest.strip_prefix('(')?;
    let close = rest.rfind(')')?;
    Some(rest[..close].trim().to_string())
}

/// A `cfg` predicate on the host build: `None` when it depends on the
/// build (`test`, a `feature` when `features` is unknown) or is malformed.
fn evaluate(predicate: &str, features: Features<'_>) -> Active {
    let predicate = predicate.trim();
    if predicate.is_empty() {
        return None;
    }
    if let Some((name, args)) = call(predicate) {
        let parts = split_args(args);
        return match name {
            "all" => all_of(parts.iter().map(|part| evaluate(part, features))),
            "any" => any_of(parts.iter().map(|part| evaluate(part, features))),
            "not" if parts.len() == 1 => evaluate(parts[0], features).map(|value| !value),
            _ => None,
        };
    }
    if let Some((key, value)) = predicate.split_once('=') {
        let key = key.trim();
        let value = value.trim().trim_matches('"');
        return Some(match key {
            "feature" => match features {
                Features::Unknown => return None,
                Features::Default(enabled) => enabled.contains(value),
            },
            "target_os" => value == "linux",
            "target_family" => value == "unix",
            "target_arch" => value == "x86_64",
            "target_pointer_width" => value == "64",
            "target_endian" => value == "little",
            "target_env" => value == "gnu",
            "target_vendor" => value == "unknown",
            "panic" => value == "unwind",
            "target_has_atomic" => true,
            _ => false,
        });
    }
    match predicate {
        "test" => None,
        "unix" | "debug_assertions" => Some(true),
        // Anything else nobody passed to rustc: `loom`, `miri`, `windows`.
        _ => Some(false),
    }
}

/// `name(args)` -> (`name`, `args`).
fn call(predicate: &str) -> Option<(&str, &str)> {
    let open = predicate.find('(')?;
    let name = predicate[..open].trim();
    let rest = predicate[open + 1..].trim_end();
    let args = rest.strip_suffix(')')?;
    (!name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
    .then_some((name, args))
}

/// `a, all(b, c), d = "x,y"` split at its top-level commas.
fn split_args(args: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut start = 0;
    for (index, byte) in args.bytes().enumerate() {
        match byte {
            b'"' => in_string = !in_string,
            b'(' if !in_string => depth += 1,
            b')' if !in_string => depth -= 1,
            b',' if !in_string && depth == 0 => {
                parts.push(args[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    let last = args[start..].trim();
    if !last.is_empty() {
        parts.push(last);
    }
    parts
}

/// The default features of the package `file` belongs to, expanded
/// (`default = ["std"]`, `std = ["alloc"]`), read once per manifest.
fn default_features(context: &dyn ResolutionContext, file: &str) -> Arc<HashSet<String>> {
    let Some(manifest) = manifest_of(context, file) else {
        return Arc::default();
    };
    let derived = context.get_rust_file_derived(&manifest, "rust-default-features", &mut || {
        let features = context
            .read_file(&manifest)
            .map(|text| expand_defaults(&text))
            .unwrap_or_default();
        Arc::new(features)
    });
    derived.downcast::<HashSet<String>>().unwrap_or_default()
}

/// The nearest `Cargo.toml` at or above `file`'s crate directory.
fn manifest_of(context: &dyn ResolutionContext, file: &str) -> Option<String> {
    let crate_key = module_location(file).crate_key;
    let mut dir = crate_key
        .rsplit_once('/')
        .map_or_else(String::new, |(dir, _)| dir.to_string());
    for _ in 0..8 {
        let manifest = if dir.is_empty() {
            "Cargo.toml".to_string()
        } else {
            format!("{dir}/Cargo.toml")
        };
        if context.file_exists(&manifest) {
            return Some(manifest);
        }
        if dir.is_empty() {
            return None;
        }
        dir = dir
            .rsplit_once('/')
            .map_or_else(String::new, |(parent, _)| parent.to_string());
    }
    None
}

/// `[features] default` and what it enables, transitively; `dep:x` and
/// `x/feature` enable dependencies, not features of this crate.
fn expand_defaults(manifest: &str) -> HashSet<String> {
    use toml::de::{DeTable, DeValue};
    let Ok(parsed) = DeTable::parse(manifest) else {
        return HashSet::new();
    };
    let Some(table) = parsed
        .get_ref()
        .iter()
        .find_map(|(key, value)| match value.get_ref() {
            DeValue::Table(table) if key.get_ref() == "features" => Some(table),
            _ => None,
        })
    else {
        return HashSet::new();
    };
    let implied_by = |feature: &str| -> Vec<String> {
        table
            .iter()
            .find_map(|(key, value)| match value.get_ref() {
                DeValue::Array(items) if key.get_ref() == feature => Some(
                    items
                        .iter()
                        .filter_map(|item| match item.get_ref() {
                            DeValue::String(text) => Some(text.to_string()),
                            _ => None,
                        })
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_default()
    };
    let mut enabled: HashSet<String> = HashSet::new();
    let mut queue: Vec<String> = vec!["default".to_string()];
    while let Some(feature) = queue.pop() {
        for entry in implied_by(&feature) {
            if entry.starts_with("dep:") || entry.contains('/') {
                continue;
            }
            if enabled.insert(entry.clone()) {
                queue.push(entry);
            }
        }
    }
    enabled
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evaluate_host(predicate: &str) -> Active {
        evaluate(predicate, Features::Unknown)
    }

    #[test]
    fn default_features_decide_what_the_host_leaves_tied() {
        let defaults = expand_defaults(
            "[features]\ndefault = [\"std\"]\nstd = [\"alloc\", \"dep:x\", \"y/z\"]\nalloc = []\nparking_lot = []\n",
        );
        let mut sorted: Vec<&String> = defaults.iter().collect();
        sorted.sort();
        assert_eq!(sorted, ["alloc", "std"]);
        let lot = "all(feature = \"parking_lot\", not(miri))";
        assert_eq!(evaluate(lot, Features::Default(&defaults)), Some(false));
        assert_eq!(
            evaluate(&format!("not({lot})"), Features::Default(&defaults)),
            Some(true)
        );
        assert_eq!(
            evaluate("feature = \"std\"", Features::Default(&defaults)),
            Some(true)
        );
        let tied = vec![
            (
                "lot",
                BuildCfg {
                    host: None,
                    default: Some(false),
                },
            ),
            (
                "std",
                BuildCfg {
                    host: None,
                    default: Some(true),
                },
            ),
            (
                "loom",
                BuildCfg {
                    host: Some(false),
                    default: Some(true),
                },
            ),
        ];
        let kept: Vec<&str> = keep_built(tied, |(_, build)| *build)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(kept, ["std"]);
    }

    #[test]
    fn the_host_build_turns_unconfigured_cfgs_off() {
        assert_eq!(evaluate_host("loom"), Some(false));
        assert_eq!(evaluate_host("all(test, loom)"), Some(false));
        assert_eq!(evaluate_host("not(all(test, loom))"), Some(true));
        assert_eq!(evaluate_host("unix"), Some(true));
        assert_eq!(evaluate_host("windows"), Some(false));
        assert_eq!(evaluate_host("target_os = \"linux\""), Some(true));
        assert_eq!(
            evaluate_host("any(target_os = \"macos\", windows)"),
            Some(false)
        );
        assert_eq!(evaluate_host("not(miri)"), Some(true));
    }

    #[test]
    fn test_and_features_stay_undecided() {
        assert_eq!(evaluate_host("test"), None);
        assert_eq!(evaluate_host("not(test)"), None);
        let lot = "all(feature = \"parking_lot\", not(miri))";
        assert_eq!(evaluate_host(lot), None);
        assert_eq!(evaluate_host(&format!("not({lot})")), None);
        assert_eq!(evaluate_host("all(feature = \"rt\", loom)"), Some(false));
    }

    #[test]
    fn reads_the_cfg_attributes_above_each_item() {
        let source = "\
#[cfg(not(all(test, loom)))]
mod std;
#[cfg(all(test, loom))]
/// Mocks.
#[allow(unused)]
mod mocked;
#[cfg_attr(docsrs, doc(cfg(feature = \"rt\")))]
pub fn plain() {}
#[cfg(any(
    feature = \"a\",
    feature = \"b\",
))]
use a::B;
";
        let cfgs = scan(&Arc::from(source));
        assert_eq!(cfgs.by_line[&2], ["not(all(test, loom))"]);
        assert_eq!(cfgs.by_line[&6], ["all(test, loom)"]);
        assert!(!cfgs.by_line.contains_key(&8));
        assert_eq!(cfgs.by_line[&13].len(), 1);
        assert_eq!(evaluate_host(&cfgs.by_line[&13][0]), None);
    }

    #[test]
    fn keeps_every_alternative_unless_the_build_rules_some_out() {
        assert_eq!(keep_active(vec![1, 2], |_| None), [1, 2]);
        assert_eq!(keep_active(vec![1, 2], |n| Some(*n == 2)), [2]);
        assert_eq!(keep_active(vec![1, 2], |_| Some(false)), [1, 2]);
        assert_eq!(keep_active(vec![1], |_| Some(false)), [1]);
    }
}

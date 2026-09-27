//! Running a compiled rule over one parsed file: raw pattern matches, the
//! `where` predicates, `ignore-patterns`, `unique`/`limit`.
//!
//! A match is a set of named captures plus two ranges: its *span* (the
//! range covering its captures — for weggli, not counting the function the
//! pattern is anchored in) and where it is *reported* (`at`, or by default the last
//! capture of a weggli match — the later statements are the sink — and the
//! first of a query match). An ignore-pattern match drops every check match
//! reported inside its span.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

use tree_sitter::{Node, QueryCursor, StreamingIterator, Tree};

use super::compile::{Backend, Pattern, PredicateKind, Rule};
use super::lang::{self, LangRules};
use super::semantics::{FunctionFacts, Semantics};
use crate::types::Language;

/// A file as the engine reads it.
pub(super) struct FileInput<'a> {
    pub path: &'a str,
    pub language: Language,
    pub source: &'a str,
    pub tree: &'a Tree,
    /// Facts per function node (by start byte), computed once per file.
    pub facts: RefCell<HashMap<usize, FunctionFacts>>,
}

impl<'a> FileInput<'a> {
    pub fn new(path: &'a str, language: Language, source: &'a str, tree: &'a Tree) -> Self {
        Self {
            path,
            language,
            source,
            tree,
            facts: RefCell::new(HashMap::new()),
        }
    }

    fn function_facts(
        &self,
        semantics: &dyn Semantics,
        rules: &LangRules,
        function: Node,
    ) -> FunctionFacts {
        if let Some(facts) = self.facts.borrow().get(&function.start_byte()) {
            return facts.clone();
        }
        let facts = semantics.function(self, rules, function);
        self.facts
            .borrow_mut()
            .insert(function.start_byte(), facts.clone());
        facts
    }
}

/// One accepted match of a check pattern.
#[derive(Debug, Clone)]
pub(super) struct Hit {
    /// Index into `rule.checks`.
    pub pattern: usize,
    /// Named captures, first occurrence of each, in pattern order.
    pub captures: Vec<(String, Range<usize>)>,
    pub span: Range<usize>,
    pub at: Range<usize>,
    /// Facts the predicates established (what a call resolved to…), as
    /// evidence notes.
    pub notes: Vec<String>,
}

impl Hit {
    pub fn capture(&self, name: &str) -> Option<&Range<usize>> {
        self.captures
            .iter()
            .find(|(capture, _)| capture == name)
            .map(|(_, range)| range)
    }
}

/// Why a raw match was not reported (for `--check` traces).
#[derive(Debug, Clone)]
pub(super) struct Rejection {
    pub pattern: String,
    pub line: u32,
    pub reason: String,
}

/// What a rule found in a file, and what it turned down.
#[derive(Default)]
pub(super) struct FileResult {
    pub hits: Vec<Hit>,
    pub rejected: Vec<Rejection>,
}

/// Run `rule` over `file`. `trace` keeps the rejections.
pub(super) fn run_rule(
    rule: &Rule,
    file: &FileInput,
    semantics: &dyn Semantics,
    trace: bool,
) -> FileResult {
    let mut result = FileResult::default();
    let rules = lang::for_language(file.language);
    for (index, pattern) in rule.checks.iter().enumerate() {
        for hit in raw_hits(pattern, index, file, rules) {
            match check_predicates(pattern, &hit, file, rules, semantics) {
                Ok(notes) => result.hits.push(Hit { notes, ..hit }),
                Err(reason) if trace => result.rejected.push(Rejection {
                    pattern: pattern.label.clone(),
                    line: position(file.tree, hit.at.start).0,
                    reason,
                }),
                Err(_) => {}
            }
        }
    }
    if result.hits.is_empty() || rule.ignores.is_empty() {
        return result;
    }
    let mut ignore_spans: Vec<(Range<usize>, &str)> = Vec::new();
    for (index, pattern) in rule.ignores.iter().enumerate() {
        for hit in raw_hits(pattern, index, file, rules) {
            if check_predicates(pattern, &hit, file, rules, semantics).is_ok() {
                ignore_spans.push((hit.span.clone(), pattern.label.as_str()));
            }
        }
    }
    let hits = std::mem::take(&mut result.hits);
    for hit in hits {
        let ignored_by = ignore_spans
            .iter()
            .find(|(span, _)| span.start <= hit.at.start && hit.at.end <= span.end);
        match ignored_by {
            Some((_, label)) => {
                if trace {
                    result.rejected.push(Rejection {
                        pattern: rule.checks[hit.pattern].label.clone(),
                        line: position(file.tree, hit.at.start).0,
                        reason: format!("dropped by {label}"),
                    });
                }
            }
            None => result.hits.push(hit),
        }
    }
    result
}

/// 1-based line and 0-based column of a node start at byte `offset`
/// (capture ranges always start at a node), from the tree: O(depth).
pub(super) fn position(tree: &Tree, offset: usize) -> (u32, u32) {
    let point = tree
        .root_node()
        .descendant_for_byte_range(offset, offset)
        .map_or(tree.root_node().start_position(), |node| {
            if node.start_byte() == offset {
                node.start_position()
            } else {
                // Inside a token: count from its start on the same line.
                let mut point = node.start_position();
                point.column += offset - node.start_byte();
                point
            }
        });
    (point.row as u32 + 1, point.column as u32)
}

/// The pattern's matches in `file`, before predicates.
fn raw_hits(pattern: &Pattern, index: usize, file: &FileInput, rules: &LangRules) -> Vec<Hit> {
    let Some(backend) = pattern.backend(file.language) else {
        return Vec::new();
    };
    let mut hits = match backend {
        Backend::Weggli(tree) => {
            if !pattern
                .identifiers
                .iter()
                .all(|identifier| file.source.contains(identifier.as_str()))
            {
                return Vec::new();
            }
            tree.matches(file.tree.root_node(), file.source)
                .into_iter()
                .filter_map(|result| {
                    // The span covers the captures (not the whole function
                    // the pattern is anchored in), so an ignore-pattern
                    // drops only what its own match covers.
                    let function = result.function_range();
                    let inner = || {
                        result
                            .captures
                            .iter()
                            .map(|capture| capture.range.clone())
                            .filter(|range| *range != function)
                    };
                    let span = match (inner().map(|r| r.start).min(), inner().map(|r| r.end).max())
                    {
                        (Some(start), Some(end)) => start..end,
                        _ => function.clone(),
                    };
                    let mut vars: Vec<(&String, &usize)> = result.vars.iter().collect();
                    vars.sort_by_key(|(_, index)| **index);
                    let captures: Vec<(String, Range<usize>)> = vars
                        .into_iter()
                        .map(|(var, index)| {
                            (
                                var.trim_start_matches('$').to_string(),
                                result.captures[*index].range.clone(),
                            )
                        })
                        .collect();
                    let at = match &pattern.at {
                        Some(name) => captures
                            .iter()
                            .find(|(capture, _)| capture == name)
                            .map(|(_, range)| range.clone())?,
                        // The last capture inside the match (the sink).
                        None => inner()
                            .max_by_key(|range| (range.start, std::cmp::Reverse(range.end)))
                            .unwrap_or_else(|| span.clone()),
                    };
                    Some(Hit {
                        pattern: index,
                        captures,
                        span,
                        at,
                        notes: Vec::new(),
                    })
                })
                .collect::<Vec<_>>()
        }
        Backend::Query(query) => {
            let names = query.capture_names();
            let mut cursor = QueryCursor::new();
            let mut matches = cursor.matches(query, file.tree.root_node(), file.source.as_bytes());
            let mut hits = Vec::new();
            'matches: while let Some(m) = matches.next() {
                let mut captures: Vec<(String, Range<usize>)> = Vec::new();
                for capture in m.captures {
                    let name = names[capture.index as usize];
                    if !captures.iter().any(|(n, _)| n == name) {
                        captures.push((name.to_string(), capture.node.byte_range()));
                    }
                }
                if captures.is_empty() {
                    continue;
                }
                for constraint in &pattern.constraints {
                    let text = captures
                        .iter()
                        .find(|(name, _)| *name == constraint.capture)
                        .map(|(_, range)| &file.source[range.clone()]);
                    let Some(text) = text else {
                        continue 'matches;
                    };
                    if constraint.regex.is_match(text) == constraint.negative {
                        continue 'matches;
                    }
                }
                let start = captures.iter().map(|(_, r)| r.start).min().unwrap_or(0);
                let end = captures.iter().map(|(_, r)| r.end).max().unwrap_or(0);
                let at = match &pattern.at {
                    Some(name) => match captures.iter().find(|(capture, _)| capture == name) {
                        Some((_, range)) => range.clone(),
                        None => continue,
                    },
                    None => captures
                        .iter()
                        .map(|(_, range)| range.clone())
                        .min_by_key(|range| (range.start, std::cmp::Reverse(range.end)))
                        .unwrap_or(start..end),
                };
                hits.push(Hit {
                    pattern: index,
                    captures,
                    span: start..end,
                    at,
                    notes: Vec::new(),
                });
            }
            hits
        }
    };

    let mut seen = HashSet::new();
    hits.retain(|hit| {
        seen.insert((
            hit.at.clone(),
            hit.captures
                .iter()
                .map(|(_, range)| range.clone())
                .collect::<Vec<_>>(),
        ))
    });
    if pattern.unique {
        hits.retain(|hit| {
            let mut texts = HashSet::new();
            hit.captures
                .iter()
                .all(|(_, range)| texts.insert(&file.source[range.clone()]))
        });
    }
    if pattern.limit {
        let root = file.tree.root_node();
        let mut functions = HashSet::new();
        hits.retain(|hit| {
            let key = node_at(root, &hit.at)
                .and_then(|node| lang::enclosing_function(rules, node))
                .map_or(hit.span.start, |function| function.start_byte());
            functions.insert(key)
        });
    }
    hits
}

/// The smallest node covering `range`.
pub(super) fn node_at<'t>(root: Node<'t>, range: &Range<usize>) -> Option<Node<'t>> {
    root.descendant_for_byte_range(range.start, range.end)
}

/// `Ok(notes)` when every predicate holds, else why one did not.
fn check_predicates(
    pattern: &Pattern,
    hit: &Hit,
    file: &FileInput,
    rules: &LangRules,
    semantics: &dyn Semantics,
) -> Result<Vec<String>, String> {
    let mut notes = Vec::new();
    let root = file.tree.root_node();
    let mut function_facts: Option<Option<FunctionFacts>> = None;
    for predicate in &pattern.predicates {
        let fail = |detail: String| Err(format!("{} rejected it: {detail}", predicate.label));
        match &predicate.kind {
            PredicateKind::Text {
                capture,
                regex,
                negative,
            } => {
                let text = hit
                    .capture(capture)
                    .map_or("", |range| &file.source[range.clone()]);
                if regex.is_match(text) == *negative {
                    return fail(format!("`{capture}` is `{}`", one_line(text, 80)));
                }
            }
            PredicateKind::Resolves {
                capture,
                regex,
                negative,
            } => {
                let call = hit
                    .capture(capture)
                    .and_then(|range| node_at(root, range))
                    .and_then(|node| lang::enclosing_call(rules, node));
                let targets = call
                    .map(|call| semantics.call_targets(file, rules, call))
                    .unwrap_or_default();
                let matched = targets.iter().find(|target| regex.is_match(target));
                match (matched, negative) {
                    (Some(target), false) => {
                        notes.push(format!("`{capture}` resolves to {target}"));
                    }
                    (None, true) => {}
                    (Some(target), true) => {
                        return fail(format!("the call resolves to {target}"));
                    }
                    (None, false) if call.is_none() => {
                        return fail(format!("`{capture}` is not in a call"));
                    }
                    (None, false) if targets.is_empty() => {
                        return fail("the call resolves to nothing".to_string());
                    }
                    (None, false) => {
                        return fail(format!("the call resolves to {}", targets.join(", ")));
                    }
                }
            }
            PredicateKind::Enclosing {
                calls,
                calls_not,
                name,
                is_test,
            } => {
                let facts = function_facts.get_or_insert_with(|| {
                    node_at(root, &hit.at)
                        .and_then(|node| lang::enclosing_function(rules, node))
                        .map(|function| file.function_facts(semantics, rules, function))
                });
                let Some(facts) = facts else {
                    return fail("the match is not inside a function".to_string());
                };
                if let Some(regex) = calls {
                    match facts.calls.iter().find(|target| regex.is_match(target)) {
                        Some(target) => notes.push(format!("{} calls {target}", facts.name)),
                        None => return fail(format!("{} has no such call", facts.name)),
                    }
                }
                let forbidden = calls_not
                    .as_ref()
                    .and_then(|regex| facts.calls.iter().find(|target| regex.is_match(target)));
                if let Some(target) = forbidden {
                    return fail(format!("{} calls {target}", facts.name));
                }
                if name.as_ref().is_some_and(|regex| {
                    !regex.is_match(&facts.name) && !regex.is_match(&facts.qualified_name)
                }) {
                    return fail(format!("the function is {}", facts.qualified_name));
                }
                if let Some(expected) = is_test.filter(|expected| facts.is_test != *expected) {
                    return fail(format!(
                        "{} is {}test code",
                        facts.name,
                        if expected { "not " } else { "" }
                    ));
                }
            }
            PredicateKind::Inside {
                capture,
                negative,
                queries,
            } => {
                let Some(query) = queries
                    .iter()
                    .find(|(language, _)| *language == file.language)
                    .map(|(_, query)| query)
                else {
                    continue;
                };
                let range = match capture {
                    Some(capture) => hit.capture(capture).cloned(),
                    None => Some(hit.at.clone()),
                };
                let found = range
                    .and_then(|range| node_at(root, &range))
                    .and_then(|node| matching_ancestor(query, node, file.source));
                match (found, negative) {
                    (Some(_), false) | (None, true) => {}
                    (Some(ancestor), true) => {
                        return fail(format!(
                            "it is inside `{}` (line {})",
                            one_line(&file.source[ancestor.byte_range()], 60),
                            ancestor.start_position().row + 1
                        ));
                    }
                    (None, false) => return fail("no enclosing node matches".to_string()),
                }
            }
        }
    }
    Ok(notes)
}

/// The nearest strict ancestor of `node` a pattern of `query` matches
/// (rooted at that ancestor). Bounded by the tree's depth; each check is
/// a cursor limited to the ancestor itself.
fn matching_ancestor<'t>(
    query: &tree_sitter::Query,
    node: Node<'t>,
    source: &str,
) -> Option<Node<'t>> {
    let mut cursor = QueryCursor::new();
    cursor.set_max_start_depth(Some(0));
    let mut current = node.parent();
    while let Some(ancestor) = current {
        let mut matches = cursor.matches(query, ancestor, source.as_bytes());
        if matches.next().is_some() {
            return Some(ancestor);
        }
        current = ancestor.parent();
    }
    None
}

/// `text` on one line, at most `max` chars.
pub(super) fn one_line(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        let cut: String = flat.chars().take(max).collect();
        format!("{cut}…")
    } else {
        flat
    }
}

/// The finding text: `message` with `{capture}` replaced by the capture's
/// code and `{function}` by the enclosing function's name.
pub(super) fn render_message(
    template: &str,
    hit: &Hit,
    source: &str,
    function: Option<&str>,
) -> String {
    let values: HashMap<&str, String> = hit
        .captures
        .iter()
        .map(|(name, range)| (name.as_str(), one_line(&source[range.clone()], 80)))
        .collect();
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let close = after.find('}');
        let name = close.map(|close| &after[..close]);
        let value = name.and_then(|name| {
            if name == "function" {
                function.map(str::to_string)
            } else {
                values.get(name).cloned()
            }
        });
        match (value, close) {
            (Some(value), Some(close)) => {
                out.push_str(&value);
                rest = &after[close + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

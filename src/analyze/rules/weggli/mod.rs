/*
Copyright 2021 Google LLC

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

     https://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.
*/
//! weggli's query compiler and matcher, ported from weggli 0.2.5
//! (github.com/weggli-rs/weggli, `src/lib.rs`, `builder.rs`, `query.rs`,
//! `result.rs`, `capture.rs`, `util.rs`; Apache-2.0 — see `NOTICE`) to the
//! tree-sitter runtime and C/C++ grammars this crate links. weggli itself
//! pins tree-sitter 0.20 and one binary can hold only one tree-sitter, so it
//! cannot be a dependency.
//!
//! A weggli pattern is C/C++ with a few extras: `$x` variables (equal text
//! wherever they recur), `_` wildcards, `_(e)` sub-expressions, `not:`
//! negative sub queries, `strict:` exact statements; patterns match
//! greedily (`$x = 10;` also matches `int x = 10;`, `f(a)` also
//! `if (f(a, b))`). Modifications to the matcher are listed at the top of
//! each file; the command line, Python bindings and the result display are
//! not ported. Errors carry no terminal colours and name where the pattern
//! fails to parse.

pub mod builder;
mod capture;
pub mod query;
pub mod result;
#[cfg(test)]
mod tests;
mod util;

use std::collections::HashMap;

use query::QueryTree;
use regex::Regex;
use tree_sitter::{Parser, Query, Tree};

use crate::extraction::grammar_language;
use crate::types::Language;

#[derive(Debug, Clone)]
pub struct QueryError {
    pub message: String,
    /// 1-based (line, column) in the pattern of a syntax error, when known
    /// (added in the port, for located rule-file errors).
    pub position: Option<(usize, usize)>,
}

impl QueryError {
    pub(crate) fn new(message: String) -> Self {
        Self {
            message,
            position: None,
        }
    }
}

fn ts_language(cpp: bool) -> tree_sitter::Language {
    grammar_language(if cpp { Language::Cpp } else { Language::C })
        .expect("the C and C++ grammars are always linked")
}

/// Helper function to parse an input string into a tree-sitter tree. This
/// function won't fail but the returned Tree might be invalid and contain
/// errors.
pub fn parse(source: &str, cpp: bool) -> Tree {
    let mut parser = get_parser(cpp);
    parser
        .parse(source, None)
        .expect("parsing without a timeout or cancellation flag always yields a tree")
}

pub fn get_parser(cpp: bool) -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&ts_language(cpp))
        .expect("the linked C/C++ grammars match the tree-sitter runtime");
    parser
}

// Internal helper function to create a new tree-sitter query.
fn ts_query(sexpr: &str, cpp: bool) -> Result<tree_sitter::Query, QueryError> {
    Query::new(&ts_language(cpp), sexpr).map_err(|e| {
        QueryError::new(format!(
            "this pattern compiles to a tree-sitter query the {} grammar rejects ({:?}: {}). \
             Simplify the pattern around that construct.",
            if cpp { "C++" } else { "C" },
            e.kind,
            e.message
        ))
    })
}

/// Map from variable names to a positive/negative regex constraint
/// (`--regex` in weggli: `var=re` must match, `var!=re` must not).
#[derive(Clone, Debug)]
pub struct RegexMap(HashMap<String, (bool, Regex)>);

impl RegexMap {
    pub fn new(m: HashMap<String, (bool, Regex)>) -> RegexMap {
        RegexMap(m)
    }

    pub fn variables(&self) -> impl Iterator<Item = &String> {
        self.0.keys()
    }

    pub fn get(&self, variable: &str) -> Option<(bool, Regex)> {
        self.0.get(variable).map(|(b, r)| (*b, r.to_owned()))
    }
}

/// Translate the search pattern in `pattern` into a weggli QueryTree.
/// `is_cpp` enables C++ mode. `force_query` can be used to allow queries with syntax errors.
/// We support some basic normalization (adding { } around queries).
pub fn parse_search_pattern(
    pattern: &str,
    is_cpp: bool,
    force_query: bool,
    regex_constraints: Option<RegexMap>,
) -> Result<QueryTree, QueryError> {
    // C++ reads `not` as the `!` operator; weggli's label is
    // case-insensitive, so spell it `NOT:` (same length: positions hold).
    let pattern = &NOT_LABEL.replace_all(pattern, "${1}NOT${2}:${3}");
    let mut tree = parse(pattern, is_cpp);
    let mut p = pattern.to_string();

    // Try to fix missing ';' at the end of a query.
    // weggli 'memcpy(a,b,size)' should work.
    if tree.root_node().has_error() && !pattern.trim_end().ends_with(';') {
        let fixed = format!("{};", pattern.trim_end());
        let fixed_tree = parse(&fixed, is_cpp);
        if !fixed_tree.root_node().has_error() {
            tree = fixed_tree;
            p = fixed;
        }
    }

    // Try to do query normalization to support missing { }
    // 'memcpy(_);' -> {memcpy(_);}
    if !tree.root_node().has_error() {
        let wrap = tree
            .root_node()
            .named_child(0)
            .is_some_and(|n| !VALID_NODE_KINDS.contains(&n.kind()))
            || tree.root_node().named_child_count() > 1;
        if wrap {
            let fixed = format!("{{{p}}}");
            let fixed_tree = parse(&fixed, is_cpp);
            if !fixed_tree.root_node().has_error() {
                tree = fixed_tree;
                p = fixed;
            }
        }
    }

    let mut c = validate_query(&tree, &p, force_query)?;

    builder::build_query_tree(&p, &mut c, is_cpp, regex_constraints)
}

static NOT_LABEL: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"(^|[\s;{}])not(\s*):([^:]|$)").expect("valid regex"));

/// Supported root node types.
const VALID_NODE_KINDS: &[&str] = &[
    "compound_statement",
    "function_definition",
    "struct_specifier",
    "enum_specifier",
    "union_specifier",
    "class_specifier",
];

/// 1-based line and column of byte `offset` in `text`.
fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..offset.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let col = before.len() - before.rfind('\n').map_or(0, |i| i + 1) + 1;
    (line, col)
}

/// Validates the user supplied search query and returns an error that
/// names the first syntax error when it doesn't parse or isn't rooted in one
/// of `VALID_NODE_KINDS`. If `force` is true, syntax errors are ignored.
/// Returns a cursor to the root node.
fn validate_query<'a>(
    tree: &'a tree_sitter::Tree,
    query: &str,
    force: bool,
) -> Result<tree_sitter::TreeCursor<'a>, QueryError> {
    if tree.root_node().has_error() && !force {
        let mut message = String::from("the pattern does not parse");
        let mut position = None;
        let mut cursor = tree.root_node().walk();

        let mut first_error = None;
        loop {
            let node = cursor.node();
            if node.has_error() {
                if node.is_error() || node.is_missing() {
                    first_error = Some(node);
                    break;
                } else if !cursor.goto_first_child() {
                    break;
                }
            } else if !cursor.goto_next_sibling() {
                break;
            }
        }

        if let Some(node) = first_error {
            let (line, col) = line_col(query, node.start_byte());
            position = Some((line, col));
            if node.is_missing() {
                message.push_str(&format!(
                    ": missing `{}` at pattern line {line}, column {col}",
                    node.kind()
                ));
            } else {
                let text: String = query[node.byte_range()].chars().take(60).collect();
                message.push_str(&format!(
                    ": unexpected `{}` at pattern line {line}, column {col}",
                    text.trim()
                ));
            }
        }
        message.push_str(
            ". A weggli pattern is C/C++ code: statements inside `{ … }`, or one function \
             definition; `$x` variables, `_` wildcards, `not:`/`strict:` labels.",
        );

        return Err(QueryError { message, position });
    }

    let mut c = tree.walk();

    if c.node().named_child_count() > 1 {
        return Err(QueryError::new(
            "the pattern has several root nodes; wrap statements in one `{ … }`".to_string(),
        ));
    }

    c.goto_first_child();

    if !VALID_NODE_KINDS.contains(&c.node().kind()) {
        return Err(QueryError::new(format!(
            "a pattern must be a `{{ … }}` block or a function/struct/enum/union/class \
             definition, not a `{}`",
            c.node().kind()
        )));
    }

    Ok(c)
}

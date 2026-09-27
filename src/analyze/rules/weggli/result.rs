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
// Ported from weggli 0.2.5 (github.com/weggli-rs/weggli, src/result.rs) to
// codegraph-rs. Modifications: std `HashMap` instead of `FxHashMap`; the
// terminal display helper (`display`, `DisplayHelper`) and the cross-file
// `chainable`/`merge_results` helpers are not ported — findings are rendered
// by the rules engine.

use std::collections::HashMap;
use std::ops::Range;

/// Struct for storing (partial) query matches.
/// We really don't want to keep track of tree-sitter AST lifetimes so
/// we do not store full nodes, but only their source range.
#[derive(Debug, Eq, PartialEq)]
pub struct QueryResult {
    // for each captured node we store the offset ranges of its src location
    pub captures: Vec<CaptureResult>,
    // Mapping from Variables to index in `captures`
    pub vars: HashMap<String, usize>,
    // Range of the outermost node. This is badly named as it does not have to be a
    // function definition, but for final query results it normally is.
    function: Range<usize>,
}

/// Stores the result (== source range) for a single capture.
/// We also store the corresponding query id and capture index
/// to make it possible to look up the result for a certain capture
/// index (see QueryResult::get_capture_result)
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureResult {
    pub range: Range<usize>,
    pub query_id: usize,
    pub capture_idx: u32,
}

impl<'b> QueryResult {
    pub fn new(
        captures: Vec<CaptureResult>,
        vars: HashMap<String, usize>,
        function: Range<usize>,
    ) -> QueryResult {
        QueryResult {
            captures,
            vars,
            function,
        }
    }

    pub fn start_offset(&self) -> usize {
        self.function.start
    }

    /// Byte range of the outermost matched node (normally the function).
    pub fn function_range(&self) -> Range<usize> {
        self.function.clone()
    }

    /// Return the captured value for a variable.
    pub fn value(&self, var: &str, source: &'b str) -> Option<&'b str> {
        self.vars
            .get(var)
            .map(|i| &source[self.captures[*i].range.clone()])
    }

    /// Try to merge two QueryResults from the same source file.
    /// The function returns None if the variable assignments for the two results differ.
    /// If `enforce_order` is set this can fail because the new ranges
    /// are not strictly after the current ranges.
    pub fn merge(
        &self,
        other: &QueryResult,
        source: &str,
        enforce_order: bool,
    ) -> Option<QueryResult> {
        let mut vars = self.vars.clone();

        let mut captures = self.captures.clone();

        if enforce_order
            && other
                .captures
                .iter()
                .any(|r| self.captures.iter().any(|r2| r.range.start <= r2.range.end))
        {
            return None;
        }

        captures.extend(other.captures.clone());

        for (k, v) in other.vars.iter() {
            match self.value(k, source) {
                None => {
                    vars.insert(k.clone(), v + self.captures.len());
                }
                Some(s) => {
                    if s != other.value(k, source).unwrap() {
                        return None;
                    }
                }
            }
        }

        Some(QueryResult::new(captures, vars, self.function.clone()))
    }

    /// Try to find the result for the capture `capture_idx` in query `query_id`
    pub fn get_capture_result(&self, query_id: usize, capture_idx: u32) -> Option<&CaptureResult> {
        self.captures
            .iter()
            .find(|c| c.capture_idx == capture_idx && c.query_id == query_id)
    }
}

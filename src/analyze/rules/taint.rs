//! Taint rules: a finding is a sink whose value a source's value reaches
//! within one function, with no sanitizer on the way.
//!
//! Each role's patterns match like check patterns (queries or weggli, with
//! their `where` predicates); their role captures then mark syntax, which
//! [`codegraph_analysis::taint_flow`] maps onto the function's IR (lowered
//! once per function by the rules-driven lowering) and solves over
//! flow-sensitive reaching definitions. Only functions holding both a sink
//! and a source are lowered. IR call ops are joined to the index's call
//! edges ([`Semantics::call_at`]) so library models apply to
//! resolved callees; without an index (`--check`) calls resolve as written.

use std::collections::BTreeMap;
use std::ops::Range;

use codegraph_analysis::ir::IrOp;
use codegraph_analysis::taint_flow::{self, CallResolution, Mark, Propagator, TaintSpec};
use tree_sitter::Node;

use super::compile::{Pattern, Rule, TaintRule};
use super::engine::{self, FileInput, FileResult, Hit, Rejection, node_at, one_line, position};
use super::lang;
use super::semantics::Semantics;

/// How a sink's value was reached: the source, then each step.
#[derive(Debug, Clone)]
pub(crate) struct Trace {
    pub source_line: u32,
    /// The source's code, on one line.
    pub source_code: String,
    /// The source pattern's label.
    pub source_pattern: String,
    /// Lines of the definitions between source and sink, in order (each
    /// once, the source's and sink's own lines left out).
    pub hops: Vec<u32>,
}

/// A function's byte range: how marks are grouped.
type FunctionKey = (usize, usize);

/// A role match: its hit, the marked capture's range, and the function
/// it sits in (its byte range; the root's for top-level code).
struct Marked {
    hit: Hit,
    range: Range<usize>,
    label: String,
    function: Option<FunctionKey>,
}

/// Run a taint rule over `file`.
pub(super) fn run(
    rule: &Rule,
    taint: &TaintRule,
    file: &FileInput,
    semantics: &dyn Semantics,
    trace: bool,
) -> FileResult {
    let rules = lang::for_language(file.language);
    let mut result = FileResult::default();
    // Each mark's function, found once (O(depth)); code outside every
    // function (a script's top level) is the root's.
    let root = file.tree.root_node();
    let function_of = |range: &Range<usize>| -> Option<Node> {
        node_at(root, range).map(|node| lang::enclosing_function(rules, node).unwrap_or(root))
    };
    let key_of = |range: &Range<usize>| {
        function_of(range).map(|function| (function.start_byte(), function.end_byte()))
    };
    let marked = |hits: Vec<Hit>, capture: &str, pattern: &Pattern| -> Vec<Marked> {
        hits.into_iter()
            .filter_map(|hit| {
                let range = hit.capture(capture)?.clone();
                Some(Marked {
                    function: key_of(&range),
                    hit,
                    range,
                    label: pattern.label.clone(),
                })
            })
            .collect()
    };

    // Sinks first: without one, nothing else runs.
    let mut sinks: Vec<Marked> = Vec::new();
    for (index, pattern) in rule.checks.iter().enumerate() {
        let found = engine::pattern_hits(pattern, index, file, semantics, trace);
        result.rejected.extend(found.rejected);
        sinks.extend(marked(found.hits, &taint.sink_values[index], pattern));
    }
    if sinks.is_empty() {
        return result;
    }
    let role = |patterns: &mut dyn Iterator<Item = (&Pattern, &str)>| -> Vec<Marked> {
        patterns
            .enumerate()
            .flat_map(|(index, (pattern, capture))| {
                let found = engine::pattern_hits(pattern, index, file, semantics, false);
                marked(found.hits, capture, pattern)
            })
            .collect()
    };
    let sources = role(&mut taint.sources.iter().map(|r| (&r.pattern, r.value.as_str())));
    let sanitizers = role(
        &mut taint
            .sanitizers
            .iter()
            .map(|r| (&r.pattern, r.value.as_str())),
    );
    // (from, to, function): both ends in the same function.
    let propagators: Vec<(Range<usize>, Range<usize>, Option<FunctionKey>)> = taint
        .propagators
        .iter()
        .enumerate()
        .flat_map(|(index, p)| {
            engine::pattern_hits(&p.pattern, index, file, semantics, false)
                .hits
                .into_iter()
                .filter_map(|hit| {
                    let from = hit.capture(&p.from)?.clone();
                    let to = hit.capture(&p.to)?.clone();
                    let function = key_of(&from).filter(|key| key_of(&to) == Some(*key));
                    Some((from, to, function))
                })
                .collect::<Vec<_>>()
        })
        .collect();

    // Group by function.
    let mut by_function: BTreeMap<(usize, usize), (Node, Vec<usize>)> = BTreeMap::new();
    for (index, sink) in sinks.iter().enumerate() {
        match function_of(&sink.range) {
            Some(function) => by_function
                .entry((function.start_byte(), function.end_byte()))
                .or_insert((function, Vec::new()))
                .1
                .push(index),
            None if trace => {
                result
                    .rejected
                    .push(rejection(file, sink, "it is not inside a function".into()))
            }
            None => {}
        }
    }
    for (key, (function, sink_indices)) in by_function {
        let function_sources: Vec<&Marked> =
            sources.iter().filter(|s| s.function == Some(key)).collect();
        let name = if function.parent().is_none() {
            "the top-level code".to_string()
        } else {
            format!("`{}`", lang::function_name(function, file.source))
        };
        if function_sources.is_empty() {
            if trace {
                for &index in &sink_indices {
                    result.rejected.push(rejection(
                        file,
                        &sinks[index],
                        format!("no source matched in {name}"),
                    ));
                }
            }
            continue;
        }
        let Some(ir) = file.lowered(rules, function) else {
            if trace {
                for &index in &sink_indices {
                    result.rejected.push(rejection(
                        file,
                        &sinks[index],
                        format!("{name} could not be lowered to IR"),
                    ));
                }
            }
            continue;
        };
        let mark = |range: &Range<usize>| Mark {
            start_byte: range.start,
            end_byte: range.end,
            kind: node_at(root, range)
                .filter(|node| node.byte_range() == *range)
                .map(|node| node.kind()),
        };
        let spec = TaintSpec {
            sources: function_sources.iter().map(|s| mark(&s.range)).collect(),
            sanitizers: sanitizers
                .iter()
                .filter(|s| s.function == Some(key))
                .map(|s| mark(&s.range))
                .collect(),
            sinks: sink_indices
                .iter()
                .map(|&index| mark(&sinks[index].range))
                .collect(),
            propagators: propagators
                .iter()
                .filter(|(_, _, function)| *function == Some(key))
                .map(|(from, to, _)| Propagator {
                    from: mark(from),
                    to: mark(to),
                })
                .collect(),
        };
        // The call join: each IR call op to the index's call edge at its
        // start, picked by name. A call of a name this file defines is
        // project code even where the index could not resolve it.
        let resolve = |op: usize| -> CallResolution {
            match &ir.body[op] {
                IrOp::Call { callee, .. } if !callee.starts_with('<') => {
                    let span = ir.span(op);
                    let mut resolution = semantics.call_at(file, span.line, span.col, callee);
                    resolution.in_project |= file.defines(rules, lang::last_name(callee));
                    resolution
                }
                _ => CallResolution::default(),
            }
        };
        let ir_lang = rules.ir.unwrap_or_default();
        let flows = taint_flow::analyze(&ir, ir_lang, &spec, &resolve);
        let mut reached = vec![false; sink_indices.len()];
        for flow in flows {
            reached[flow.sink] = true;
            let sink = &sinks[sink_indices[flow.sink]];
            let source = function_sources[flow.source];
            let source_line = position(file.tree, source.range.start).0;
            let sink_line = position(file.tree, sink.range.start).0;
            let mut hops: Vec<u32> = Vec::new();
            for hop in &flow.hops {
                let line = hop.span.line;
                if line > 0 && line != source_line && line != sink_line && !hops.contains(&line) {
                    hops.push(line);
                }
            }
            let mut hit = sink.hit.clone();
            hit.at = sink.range.clone();
            hit.flow = Some(Trace {
                source_line,
                source_code: one_line(&file.source[source.range.clone()], 80),
                source_pattern: source.label.clone(),
                hops,
            });
            result.hits.push(hit);
        }
        if trace {
            for (position, &index) in sink_indices.iter().enumerate() {
                if !reached[position] {
                    result.rejected.push(rejection(
                        file,
                        &sinks[index],
                        format!(
                            "no source reaches it ({} source match{} in {name})",
                            function_sources.len(),
                            if function_sources.len() == 1 {
                                ""
                            } else {
                                "es"
                            }
                        ),
                    ));
                }
            }
        }
    }
    result
}

fn rejection(file: &FileInput, sink: &Marked, reason: String) -> Rejection {
    Rejection {
        pattern: sink.label.clone(),
        line: position(file.tree, sink.range.start).0,
        reason,
    }
}

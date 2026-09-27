//! `missing-companion-call`: callers of `A` almost always also call `B`
//! (`begin`/`commit`, `lock`/`unlock`, `record`/`flush`); a caller of `A`
//! that never calls `B` is suspect.
//!
//! Language-agnostic: read from the index's resolved calls only. Per caller
//! (a non-test function), the set of project functions it calls; per `A`,
//! how many of its callers also call each `B`. Everything is hash maps over
//! the call sites — no source is read.

use std::collections::HashMap;

use super::{CalledNames, TestRegions, z_confidence};
use crate::analyze::bugs::{Detector, Evidence, Finding, FnSpan, Project};

pub(super) const RULE: &str = "missing-companion-call";

/// Distinct callers `A` needs before its callers make a belief.
const MIN_CALLERS: usize = 5;
/// Callers of `A` that must also call `B`, and their share.
const MIN_SUPPORT: usize = 4;
const MIN_RATIO: f64 = 0.8;
/// A `B` called by more than this share of all functions is a utility
/// (logging, formatting) that co-occurs with everything.
const UBIQUITOUS: f64 = 0.2;
/// The same within a directory subtree (`src/bin/`) of at least this many
/// calling functions.
const LOCAL_UBIQUITOUS: f64 = 0.15;
const MIN_LOCAL_FUNCTIONS: usize = 20;
/// `B` must be this many times likelier among `A`'s callers than among all
/// functions (lift), or the pairing is chance.
const MIN_LIFT: f64 = 4.0;
/// Share of `B`'s callers that must call `A`.
const MIN_REVERSE: f64 = 0.5;
/// Callers this short (lines) are thin wrappers: leave pairing to theirs.
const MIN_CALLER_LINES: u32 = 4;
/// Agreeing callers listed as evidence.
const MAX_EVIDENCE: usize = 5;
/// B2 reads only co-occurrence, no data flow: rank it below the syntactic
/// templates at equal agreement.
const WEIGHT: f64 = 0.8;

pub(super) fn findings(
    project: &Project,
    test_regions: &TestRegions,
    called_names: &CalledNames,
) -> Vec<Finding> {
    // Every non-test function, by id.
    let mut spans: HashMap<&str, &FnSpan> = HashMap::new();
    for file in project.files() {
        let regions = test_regions.get(file).map_or(&[][..], Vec::as_slice);
        for span in project.functions_in(file) {
            let in_region = regions
                .iter()
                .any(|&(start, end)| start <= span.start_line && span.end_line <= end);
            if !span.is_test && !in_region {
                spans.insert(span.id.as_str(), span);
            }
        }
    }
    let calls = project.call_sites();

    // Intern function ids; per caller, its callees with the first site of each.
    let mut ids: HashMap<&str, u32> = HashMap::new();
    let mut names: Vec<&str> = Vec::new();
    let mut callees: Vec<HashMap<u32, usize>> = Vec::new();
    for (site, call) in calls.iter().enumerate() {
        if call.in_test || !matches!(call.callee_kind.as_str(), "function" | "method") {
            continue;
        }
        let (Some((caller, _)), Some((callee, _))) = (
            spans.get_key_value(call.caller_id.as_str()),
            spans.get_key_value(call.callee_id.as_str()),
        ) else {
            continue;
        };
        let caller_ix = *ids.entry(caller).or_insert_with(|| {
            names.push(caller);
            callees.push(HashMap::new());
            (names.len() - 1) as u32
        });
        let callee_ix = *ids.entry(callee).or_insert_with(|| {
            names.push(callee);
            callees.push(HashMap::new());
            (names.len() - 1) as u32
        });
        if caller_ix != callee_ix {
            callees[caller_ix as usize].entry(callee_ix).or_insert(site);
        }
    }

    let n = names.len();
    let mut callers: Vec<Vec<u32>> = vec![Vec::new(); n];
    for (caller, set) in callees.iter().enumerate() {
        for &callee in set.keys() {
            callers[callee as usize].push(caller as u32);
        }
    }
    for list in &mut callers {
        list.sort_unstable();
    }
    let total = spans.len().max(1) as f64;
    let ubiquitous = |f: u32| callers[f as usize].len() as f64 > UBIQUITOUS * total;
    let span = |f: u32| spans[names[f as usize]];

    // Per directory subtree: the functions calling anything, and each
    // callee's callers there — a helper most CLI commands call (`bold`,
    // `info`) is a utility of that tree even when rare project-wide.
    let dirs_of = |f: u32| -> Vec<&str> {
        let file = span(f).file.as_str();
        let mut dirs = vec![""];
        dirs.extend(file.match_indices('/').map(|(slash, _)| &file[..slash]));
        dirs
    };
    let mut active: HashMap<&str, usize> = HashMap::new();
    let mut local: HashMap<(u32, &str), usize> = HashMap::new();
    for (caller, set) in callees.iter().enumerate() {
        if set.is_empty() {
            continue;
        }
        for dir in dirs_of(caller as u32) {
            *active.entry(dir).or_default() += 1;
            for &callee in set.keys() {
                *local.entry((callee, dir)).or_default() += 1;
            }
        }
    }
    let locally_ubiquitous = |b: u32, caller: u32| {
        dirs_of(caller).into_iter().any(|dir| {
            let active = active.get(dir).copied().unwrap_or(0);
            active >= MIN_LOCAL_FUNCTIONS
                && local.get(&(b, dir)).copied().unwrap_or(0) as f64
                    > LOCAL_UBIQUITOUS * active as f64
        })
    };

    // (deviant caller, A) → the companions it lacks, with each belief.
    struct Missing {
        b: u32,
        support: usize,
        callers_of_a: usize,
    }
    let mut missing: HashMap<(u32, u32), Vec<Missing>> = HashMap::new();
    for a in 0..n as u32 {
        let of_a = &callers[a as usize];
        if of_a.len() < MIN_CALLERS || ubiquitous(a) {
            continue;
        }
        let mut support: HashMap<u32, usize> = HashMap::new();
        for &caller in of_a {
            for &b in callees[caller as usize].keys() {
                if b != a {
                    *support.entry(b).or_default() += 1;
                }
            }
        }
        let a_name = &span(a).name;
        let a_calls: &HashMap<u32, usize> = &callees[a as usize];
        for (b, count) in support {
            if count < MIN_SUPPORT
                || (count as f64) < MIN_RATIO * of_a.len() as f64
                || count == of_a.len()
                || ubiquitous(b)
                || &span(b).name == a_name
                // `A` calls `B` itself: its callers need not.
                || a_calls.contains_key(&b)
            {
                continue;
            }
            // `B` belongs with `A`: at least half of `B`'s callers call `A`
            // (else `B` is a helper common around `A`, not its companion).
            if (count as f64) < MIN_REVERSE * callers[b as usize].len() as f64 {
                continue;
            }
            let base = callers[b as usize].len() as f64 / total;
            let lift = (count as f64 / of_a.len() as f64) / base;
            if lift < MIN_LIFT {
                continue;
            }
            let of_b = &callers[b as usize];
            for &caller in of_a {
                if caller == b || of_b.binary_search(&caller).is_ok() {
                    continue;
                }
                // It calls `B` where the index did not resolve the call.
                let calls_b_unresolved = called_names
                    .get(names[caller as usize])
                    .is_some_and(|called| called.contains(&span(b).name));
                // `B` calls it: it is part of `B`.
                let inside_b = callees[b as usize].contains_key(&caller);
                if inside_b || calls_b_unresolved || locally_ubiquitous(b, caller) {
                    continue;
                }
                // Every caller of it calls `B`: it hands `A`'s result up
                // (`open_handler` returns the graph its callers close).
                let calls_b = |f: u32| {
                    callees[f as usize].contains_key(&b)
                        || called_names
                            .get(names[f as usize])
                            .is_some_and(|called| called.contains(&span(b).name))
                };
                let up = &callers[caller as usize];
                if !up.is_empty() && up.iter().all(|&f| calls_b(f)) {
                    continue;
                }
                let caller_span = span(caller);
                if caller_span.end_line.saturating_sub(caller_span.start_line) + 1
                    < MIN_CALLER_LINES
                {
                    continue;
                }
                // It reaches `B` one call away (through a helper): not missing.
                let via_helper = callees[caller as usize]
                    .keys()
                    .any(|&c| c != a && callees[c as usize].contains_key(&b));
                // It does `B`'s work inline: calls what `B` calls.
                let inlines_b = callees[caller as usize]
                    .keys()
                    .any(|&c| c != a && !ubiquitous(c) && callees[b as usize].contains_key(&c));
                if via_helper || inlines_b {
                    continue;
                }
                missing.entry((caller, a)).or_default().push(Missing {
                    b,
                    support: count,
                    callers_of_a: of_a.len(),
                });
            }
        }
    }

    let mut keys: Vec<(u32, u32)> = missing.keys().copied().collect();
    keys.sort_unstable_by_key(|&(caller, a)| (names[caller as usize], names[a as usize]));
    let mut out = Vec::new();
    for key in keys {
        let (caller, a) = key;
        let mut lacking = missing.remove(&key).unwrap_or_default();
        lacking.sort_by(|x, y| {
            y.support
                .cmp(&x.support)
                .then(span(x.b).name.cmp(&span(y.b).name))
        });
        let best = &lacking[0];
        let a_site = &calls[callees[caller as usize][&a]];
        let companions = lacking
            .iter()
            .take(3)
            .map(|m| format!("`{}` ({} of {})", span(m.b).name, m.support, m.callers_of_a))
            .collect::<Vec<_>>()
            .join(", ");
        let evidence: Vec<Evidence> = callers[a as usize]
            .iter()
            .filter_map(|&other| {
                callees[other as usize]
                    .get(&best.b)
                    .map(|&site| &calls[site])
            })
            .take(MAX_EVIDENCE)
            .map(|site| Evidence {
                file: site.file.clone(),
                line: site.line,
                note: format!(
                    "`{}` calls `{}` and `{}`",
                    site.caller,
                    span(a).name,
                    span(best.b).name
                ),
            })
            .collect();
        out.push(Finding {
            detector: Detector::Deviance,
            rule: RULE,
            file: a_site.file.clone(),
            line: a_site.line,
            col: a_site.col,
            function: Some(span(caller).qualified_name.clone()),
            message: format!(
                "`{}` calls `{}` but not {companions} — the callers of `{}` that call it too",
                span(caller).name,
                span(a).name,
                span(a).name,
            ),
            confidence: WEIGHT * z_confidence(best.support, best.callers_of_a),
            evidence,
        });
    }
    out
}

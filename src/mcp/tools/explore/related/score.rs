//! Score and order neighbour files.
//!
//! A deterministic linear score. Its weights come from a replay of 168 real
//! agent episodes (which files the agent edited or cited that explore had
//! missed): what separated those files from the rest of the neighbourhood was
//! how many *different* seed files and seed symbols they touch, query words
//! in their linked symbols, and the same directory as a seed — not raw edge
//! volume. Heavy one-way use of a neighbour (the seeds calling into a utility
//! or type module) and ubiquitous symbols mark hubs, which rank lower; tests
//! rank lower unless the query asks about tests.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use super::cochange::{CoChange, CoChangeEntry};
use super::gather::{Neighbour, is_generic_name};
use super::reason::describe;
use super::terms::{overlap, segments};
use super::{KeySymbol, RelatedFile};
use crate::codegraph::CodeGraph;
use crate::error::Result;
use crate::types::NodeKind;

const SEED_SPREAD: f64 = 0.45;
const SEED_SYMBOLS: f64 = 0.65;
const NEIGHBOUR_SYMBOLS: f64 = 0.3;
const OUTBOUND_MASS: f64 = -0.4;
const INBOUND_MASS: f64 = -0.1;
const TERM_IN_SYMBOLS: f64 = 1.0;
const TERM_IN_PATH: f64 = 0.4;
const MAX_TERM_HITS: usize = 3;
const SAME_DIRECTORY: f64 = 0.3;
const TEST_FILE: f64 = 0.5;
const CO_CHANGE: f64 = 0.8;
const HUB: f64 = 0.3;
/// Specificity kept by a generic seed-symbol name.
const GENERIC_SYMBOL: f64 = 0.3;
/// Symbol degree at which the hub penalty reaches `HUB * ln 2`.
const HUB_DEGREE_SCALE: f64 = 64.0;
/// Degrees are counted up to this many edges.
const HUB_DEGREE_CAP: usize = 1_000;
/// Linked symbols per candidate whose degree is counted.
const HUB_SYMBOLS: usize = 2;
/// Candidates (as a multiple of the row limit) that get the hub pass.
const SHORTLIST_FACTOR: usize = 3;
/// Seeds whose directories count for the sibling signal.
const SIBLING_SEEDS: usize = 8;
/// Seed weight decay per rank: the first seed counts 1, the fifth ~0.42.
const RANK_DECAY: f64 = 0.35;

/// Query-side inputs to scoring.
pub(super) struct ScoreContext<'a> {
    pub terms: HashSet<String>,
    pub seeds: &'a [&'a str],
    pub tests_wanted: bool,
}

/// How many neighbour files link to each seed symbol. A seed symbol half the
/// neighbourhood uses (`Result`, `unwrap`, a shared error type) says little
/// about any one of them.
pub(super) struct SeedSymbolSpread(HashMap<String, usize>);

impl SeedSymbolSpread {
    pub(super) fn of(neighbourhood: &HashMap<String, Neighbour>) -> Self {
        let mut spread: HashMap<String, usize> = HashMap::new();
        for neighbour in neighbourhood.values() {
            for symbol in &neighbour.seed_symbols {
                *spread.entry(symbol.clone()).or_default() += 1;
            }
        }
        Self(spread)
    }

    /// `1` for a seed symbol one neighbour links to, falling off with the log
    /// of how many do; generic names (`get`, `drop`) count for less still.
    pub(super) fn specificity(&self, symbol: &str) -> f64 {
        let files = self.0.get(symbol).copied().unwrap_or(1).max(1);
        let generic = if is_generic_name(symbol) {
            GENERIC_SYMBOL
        } else {
            1.0
        };
        generic / (1.0 + (files as f64).ln())
    }
}

/// Weight of the seed at `rank` in explore's file order.
pub(super) fn seed_weight(rank: usize) -> f64 {
    1.0 / (1.0 + RANK_DECAY * rank as f64)
}

struct Candidate {
    path: String,
    neighbour: Option<Neighbour>,
    cochange: Option<CoChangeEntry>,
    is_test: bool,
    same_directory: bool,
    score: f64,
}

/// Order the neighbourhood and return the best `limit` files as rows.
pub(super) fn rank(
    cg: &CodeGraph,
    ctx: &ScoreContext<'_>,
    mut neighbourhood: HashMap<String, Neighbour>,
    cochange: &CoChange,
    limit: usize,
) -> Result<Vec<RelatedFile>> {
    let sibling_dirs: HashSet<&str> = ctx
        .seeds
        .iter()
        .take(SIBLING_SEEDS)
        .map(|seed| parent_dir(seed))
        .collect();
    let spread = SeedSymbolSpread::of(&neighbourhood);
    let mut paths: Vec<String> = neighbourhood.keys().cloned().collect();
    paths.extend(indexed_cochange_files(cg, cochange, &neighbourhood, limit)?);
    let mut candidates: Vec<Candidate> = paths
        .into_iter()
        .map(|path| {
            let neighbour = neighbourhood.remove(&path);
            let is_test = crate::mcp::tools::format::is_test_path(&path)
                || crate::search::is_test_symbol(&path, "");
            let same_directory = sibling_dirs.contains(parent_dir(&path));
            let mut candidate = Candidate {
                cochange: cochange.by_file.get(&path).cloned(),
                path,
                neighbour,
                is_test,
                same_directory,
                score: 0.0,
            };
            candidate.score = base_score(ctx, &spread, &candidate);
            candidate
        })
        .collect();
    candidates.sort_by(by_score);
    candidates.truncate(limit.saturating_mul(SHORTLIST_FACTOR));
    for candidate in &mut candidates {
        crate::graph::cancel::check()?;
        candidate.score -= HUB * hub_degree(cg, candidate.neighbour.as_ref())?;
    }
    candidates.sort_by(by_score);
    candidates.truncate(limit);
    Ok(candidates
        .into_iter()
        .map(|candidate| RelatedFile {
            reason: describe(
                candidate.neighbour.as_ref(),
                candidate.cochange.as_ref(),
                ctx.seeds,
                candidate.is_test,
                candidate.same_directory,
                &spread,
            ),
            symbol: candidate.neighbour.as_ref().and_then(key_symbol),
            path: candidate.path,
        })
        .collect())
}

fn by_score(a: &Candidate, b: &Candidate) -> Ordering {
    b.score
        .total_cmp(&a.score)
        .then_with(|| a.path.cmp(&b.path))
}

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// Co-changed files with no graph link, best first, that are in the index.
fn indexed_cochange_files(
    cg: &CodeGraph,
    cochange: &CoChange,
    neighbourhood: &HashMap<String, Neighbour>,
    limit: usize,
) -> Result<Vec<String>> {
    let mut only: Vec<(&String, &CoChangeEntry)> = cochange
        .by_file
        .iter()
        .filter(|(path, _)| !neighbourhood.contains_key(*path))
        .collect();
    only.sort_by(|a, b| b.1.weight.total_cmp(&a.1.weight).then_with(|| a.0.cmp(b.0)));
    let mut out = Vec::new();
    for (path, _) in only.into_iter().take(limit) {
        if cg.get_file(path)?.is_some() {
            out.push(path.clone());
        }
    }
    Ok(out)
}

fn base_score(
    ctx: &ScoreContext<'_>,
    symbol_spread: &SeedSymbolSpread,
    candidate: &Candidate,
) -> f64 {
    let mut score = 0.0;
    if let Some(neighbour) = &candidate.neighbour {
        let spread: f64 = neighbour
            .seed_ranks
            .iter()
            .map(|rank| seed_weight(*rank))
            .sum();
        let mut seed_symbols: Vec<&String> = neighbour.seed_symbols.iter().collect();
        seed_symbols.sort();
        let seed_symbol_mass: f64 = seed_symbols
            .into_iter()
            .map(|symbol| symbol_spread.specificity(symbol))
            .sum();
        score += SEED_SPREAD * spread
            + SEED_SYMBOLS * seed_symbol_mass.ln_1p()
            + NEIGHBOUR_SYMBOLS * (neighbour.symbols.len() as f64).ln_1p()
            + OUTBOUND_MASS * neighbour.outbound_weight.ln_1p()
            + INBOUND_MASS * neighbour.inbound_weight.ln_1p();
        let mut symbol_segments = HashSet::new();
        for symbol in neighbour.symbols.values() {
            symbol_segments.extend(segments(&symbol.name));
        }
        score += TERM_IN_SYMBOLS * overlap(&ctx.terms, &symbol_segments).min(MAX_TERM_HITS) as f64;
    }
    if let Some(entry) = &candidate.cochange {
        score += CO_CHANGE * (2.0 * entry.weight).ln_1p();
    }
    score +=
        TERM_IN_PATH * overlap(&ctx.terms, &segments(&candidate.path)).min(MAX_TERM_HITS) as f64;
    if candidate.same_directory {
        score += SAME_DIRECTORY;
    }
    if candidate.is_test {
        score += if ctx.tests_wanted {
            TEST_FILE
        } else {
            -TEST_FILE
        };
    }
    score
}

/// Mean damped degree of the candidate's most-linked symbols: in-degree for a
/// symbol the seeds use, out-degree for one that uses the seeds.
fn hub_degree(cg: &CodeGraph, neighbour: Option<&Neighbour>) -> Result<f64> {
    let Some(neighbour) = neighbour else {
        return Ok(0.0);
    };
    let mut linked: Vec<(&String, &super::gather::LinkedSymbol)> =
        neighbour.symbols.iter().collect();
    linked.sort_by(|a, b| b.1.weight.total_cmp(&a.1.weight).then_with(|| a.0.cmp(b.0)));
    let mut total = 0.0;
    let mut counted = 0usize;
    for (id, symbol) in linked.into_iter().take(HUB_SYMBOLS) {
        let degree = cg.count_node_edges_capped(id, !symbol.inbound, HUB_DEGREE_CAP)?;
        total += (degree as f64 / HUB_DEGREE_SCALE).ln_1p();
        counted += 1;
    }
    Ok(if counted == 0 {
        0.0
    } else {
        total / counted as f64
    })
}

/// The neighbour symbol carrying the most link weight (earliest line on ties).
fn key_symbol(neighbour: &Neighbour) -> Option<KeySymbol> {
    neighbour
        .symbols
        .values()
        // Module-scope code links through the file (or an import) node; its
        // "line 1" says nothing a row's path doesn't.
        .filter(|symbol| !matches!(symbol.kind, NodeKind::File | NodeKind::Import))
        .max_by(|a, b| {
            a.weight
                .total_cmp(&b.weight)
                .then_with(|| b.start_line.cmp(&a.start_line))
                .then_with(|| b.name.cmp(&a.name))
        })
        .map(|symbol| KeySymbol {
            name: symbol.name.clone(),
            kind: symbol.kind,
            start_line: symbol.start_line,
            end_line: symbol.end_line,
        })
}

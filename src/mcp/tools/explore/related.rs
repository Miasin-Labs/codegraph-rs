//! Related files: the one-hop neighbourhood of explore's ranked files.
//!
//! A replay of 168 agent episodes found that 60% of the files agents went on
//! to edit or cite, and explore missed, sat one graph hop from a file explore
//! had surfaced — but a typical neighbourhood is ~75 files, and taking the
//! most-connected ones barely helps. So this module *ranks* that
//! neighbourhood (graph links to the seed files, query-term overlap, git
//! co-change, test links, same-directory siblings, a hub penalty) and hands
//! the top files back as compact rows — path, why it is related, and its key
//! symbol — with a short source window only where the budget allows.
//!
//! Everything here is bounded: each seed costs two capped SQL reads, hub
//! degrees are counted up to a cap for a short list only, and `git log` runs
//! concurrently under a hard deadline.

mod cochange;
mod gather;
mod reason;
mod score;
mod terms;
mod window;

#[cfg(test)]
mod tests;

use std::collections::HashSet;

pub(in crate::mcp::tools::explore) use cochange::CoChangeProbe;
pub(in crate::mcp::tools::explore) use window::related_windows;

use super::super::format::{ExploreOutputBudget, QUERY_MENTIONS_TESTS_RE};
use super::types::{ExploreRelatedFile, RankedExploreFiles};
use crate::codegraph::CodeGraph;
use crate::error::Result;
use crate::types::NodeKind;

/// How many of explore's ranked files seed the neighbourhood.
const SEED_FILES: usize = 12;
/// One related row per this many characters of output budget, clamped to
/// `MIN_ROWS..=MAX_ROWS` (10 rows at 13K, 20 at 24K).
const CHARS_PER_ROW: usize = 1_200;
const MIN_ROWS: usize = 8;
const MAX_ROWS: usize = 20;
/// Budget share rows (and their windows) may take from source.
const MAX_RESERVE_SHARE: f64 = 0.2;

/// Everything the neighbourhood ranker reads.
pub(in crate::mcp::tools::explore) struct RelatedRequest<'a> {
    pub cg: &'a CodeGraph,
    pub query: &'a str,
    pub ranked: &'a RankedExploreFiles,
    pub budget: ExploreOutputBudget,
}

/// A ranked neighbour, before it is rendered as a payload row.
#[derive(Clone, Debug)]
pub(in crate::mcp::tools::explore) struct RelatedFile {
    pub path: String,
    pub reason: String,
    pub symbol: Option<KeySymbol>,
}

/// The neighbour symbol most tied to the seeds, with its line span.
#[derive(Clone, Debug)]
pub(in crate::mcp::tools::explore) struct KeySymbol {
    pub name: String,
    pub kind: NodeKind,
    pub start_line: usize,
    pub end_line: usize,
}

/// Ranked neighbours plus the characters to hold back from source rendering
/// so they fit inside the explore budget.
pub(in crate::mcp::tools::explore) struct RelatedPlan {
    pub files: Vec<RelatedFile>,
    pub reserve: usize,
}

impl RelatedPlan {
    pub fn empty() -> Self {
        Self {
            files: Vec::new(),
            reserve: 0,
        }
    }

    /// The payload rows, best first.
    pub fn rows(&self) -> Vec<ExploreRelatedFile> {
        self.files
            .iter()
            .map(|file| ExploreRelatedFile {
                path: file.path.clone(),
                reason: file.reason.clone(),
                symbol: file.symbol.as_ref().map(|symbol| symbol.name.clone()),
                line: file.symbol.as_ref().map(|symbol| symbol.start_line),
            })
            .collect()
    }

    /// Markdown rendering of the rows for the human (CLI) text.
    pub fn append_markdown(&self, lines: &mut Vec<String>) {
        if self.files.is_empty() {
            return;
        }
        lines.push("### Related files — one hop from the files above".to_string());
        lines.push(String::new());
        for file in &self.files {
            let symbol = file
                .symbol
                .as_ref()
                .map(|symbol| format!(" · `{}`:{}", symbol.name, symbol.start_line))
                .unwrap_or_default();
            lines.push(format!("- {} — {}{}", file.path, file.reason, symbol));
        }
        lines.push(String::new());
    }
}

/// Row budget for an explore output budget.
fn max_rows(budget: ExploreOutputBudget) -> usize {
    (budget.max_output_chars / CHARS_PER_ROW).clamp(MIN_ROWS, MAX_ROWS)
}

/// Rank the one-hop neighbourhood of `req.ranked`'s top files. `history` is
/// the co-change read explore started when it began (see [`CoChangeProbe`]).
pub(in crate::mcp::tools::explore) fn plan_related_files(
    req: &RelatedRequest<'_>,
    history: Option<CoChangeProbe>,
) -> Result<RelatedPlan> {
    let seeds: Vec<&str> = req
        .ranked
        .sorted_files
        .iter()
        .take(SEED_FILES)
        .map(String::as_str)
        .collect();
    if seeds.is_empty() {
        return Ok(RelatedPlan::empty());
    }
    let excluded: HashSet<&str> = req.ranked.sorted_files.iter().map(String::as_str).collect();
    let neighbourhood = gather::gather(req.cg, &seeds, &excluded)?;
    let commits = history.and_then(CoChangeProbe::collect);
    let cochange = commits
        .map(|commits| cochange::CoChange::from_commits(&commits, &seeds, &excluded))
        .unwrap_or_default();
    let context = score::ScoreContext {
        terms: terms::query_terms(req.query),
        seeds: &seeds,
        tests_wanted: QUERY_MENTIONS_TESTS_RE.is_match(req.query),
    };
    let limit = max_rows(req.budget);
    let files = score::rank(req.cg, &context, neighbourhood, &cochange, limit)?;
    let rows_cost: usize = files
        .iter()
        .map(|file| file.path.len() + file.reason.len() + 64)
        .sum();
    let windowed = files
        .iter()
        .filter(|file| file.symbol.is_some())
        .count()
        .min(window::WINDOW_FILES);
    let window_cost = windowed * window::WINDOW_COST_ESTIMATE;
    let ceiling = (req.budget.max_output_chars as f64 * MAX_RESERVE_SHARE) as usize;
    Ok(RelatedPlan {
        reserve: (rows_cost + window_cost).min(ceiling),
        files,
    })
}

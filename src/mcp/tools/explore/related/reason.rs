//! One short line saying why a neighbour is related: its strongest graph
//! relation to a seed symbol, co-change with a seed file, or the directory.

use super::cochange::CoChangeEntry;
use super::gather::{Direction, Neighbour, Relation};
use super::score::SeedSymbolSpread;

/// Reasons joined per row.
const MAX_REASONS: usize = 2;

/// `calls handle_explore +3; co-changed with handler.rs in 4 commits`.
pub(super) fn describe(
    neighbour: Option<&Neighbour>,
    cochange: Option<&CoChangeEntry>,
    seeds: &[&str],
    is_test: bool,
    same_directory: bool,
    spread: &SeedSymbolSpread,
) -> String {
    let mut reasons = Vec::new();
    if let Some(neighbour) = neighbour {
        reasons.extend(graph_reason(neighbour, is_test, spread));
    }
    if let Some(entry) = cochange {
        if let Some(seed) = seeds.get(entry.seed_rank) {
            let commits = entry.commits;
            reasons.push(format!(
                "co-changed with {} in {commits} commit{}",
                file_name(seed),
                if commits == 1 { "" } else { "s" }
            ));
        }
    }
    if reasons.is_empty() && same_directory {
        reasons.push("same directory as a result".to_string());
    }
    reasons.truncate(MAX_REASONS);
    reasons.join("; ")
}

/// The relation with the most specific link mass (links × specificity ×
/// relation weight), named with its most specific seed symbol, so a symbol
/// every neighbour uses loses to one only this file uses and an import loses
/// to a call; `+N` counts the other seed symbols linked.
fn graph_reason(neighbour: &Neighbour, is_test: bool, spread: &SeedSymbolSpread) -> Option<String> {
    let mass = |name: &str, links: usize| links as f64 * spread.specificity(name);
    let ((direction, relation), symbols) =
        neighbour.by_relation.iter().max_by(|(ka, a), (kb, b)| {
            let total_a: f64 = a
                .iter()
                .map(|(name, links)| mass(name, *links))
                .sum::<f64>()
                * ka.1.weight();
            let total_b: f64 = b
                .iter()
                .map(|(name, links)| mass(name, *links))
                .sum::<f64>()
                * kb.1.weight();
            total_a.total_cmp(&total_b).then_with(|| kb.cmp(ka))
        })?;
    let (seed_symbol, _) = symbols.iter().max_by(|(name_a, a), (name_b, b)| {
        mass(name_a, **a)
            .total_cmp(&mass(name_b, **b))
            .then_with(|| name_b.cmp(name_a))
    })?;
    let verb = verb(*direction, *relation, is_test);
    let others = neighbour.seed_symbols.len().saturating_sub(1);
    Some(if others == 0 {
        format!("{verb} {seed_symbol}")
    } else {
        format!("{verb} {seed_symbol} +{others}")
    })
}

fn verb(direction: Direction, relation: Relation, is_test: bool) -> &'static str {
    match (direction, relation) {
        (Direction::Inbound, Relation::Calls | Relation::Uses) if is_test => "tests",
        (Direction::Inbound, Relation::Calls) => "calls",
        (Direction::Inbound, Relation::Uses) => "uses",
        (Direction::Inbound, Relation::Implements) => "implements",
        (Direction::Inbound, Relation::Extends) => "extends",
        (Direction::Inbound, Relation::Overrides) => "overrides",
        (Direction::Inbound, Relation::Imports) => "imports",
        (Direction::Outbound, Relation::Calls) => "called by",
        (Direction::Outbound, Relation::Uses) => "used by",
        (Direction::Outbound, Relation::Implements) => "implemented by",
        (Direction::Outbound, Relation::Extends) => "extended by",
        (Direction::Outbound, Relation::Overrides) => "overridden by",
        (Direction::Outbound, Relation::Imports) => "imported by",
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

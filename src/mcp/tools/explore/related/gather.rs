//! Collect the cross-file links of each seed file into one record per
//! neighbouring file.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::codegraph::{CodeGraph, CrossFileLink};
use crate::error::Result;
use crate::types::{EdgeKind, NodeKind};

/// Link rows read per seed file and direction.
const LINKS_PER_SEED: usize = 1_500;
/// Symbols of one seed file scanned for links (generated headers can hold
/// tens of thousands).
const LOCAL_NODES_PER_SEED: usize = 4_000;

/// Which way an edge points, seen from the seed file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum Direction {
    /// The neighbour's symbol uses the seed's (neighbour → seed).
    Inbound,
    /// The seed's symbol uses the neighbour's (seed → neighbour).
    Outbound,
}

/// A neighbour symbol linked to the seeds.
#[derive(Clone, Debug)]
pub(super) struct LinkedSymbol {
    pub name: String,
    pub kind: NodeKind,
    pub start_line: usize,
    pub end_line: usize,
    /// Kind-weighted link mass through this symbol.
    pub weight: f64,
    /// Whether any link through it points inbound (it is a caller/user).
    pub inbound: bool,
}

/// Everything known about one neighbouring file.
#[derive(Clone, Debug, Default)]
pub(super) struct Neighbour {
    /// Seed ranks (index into the seed list) this file links to. Ordered, so
    /// float sums over it are reproducible run to run.
    pub seed_ranks: BTreeSet<usize>,
    /// Distinct seed-side symbol names on the links.
    pub seed_symbols: HashSet<String>,
    /// Neighbour-side symbols on the links, by node id.
    pub symbols: HashMap<String, LinkedSymbol>,
    pub inbound_weight: f64,
    pub outbound_weight: f64,
    /// Link counts by relation and seed-side symbol, for reasons.
    pub by_relation: BTreeMap<(Direction, Relation), BTreeMap<String, usize>>,
}

/// What an edge means for a reason line, independent of its direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum Relation {
    Implements,
    Extends,
    Overrides,
    Calls,
    Uses,
    Imports,
}

impl Relation {
    /// How much a link of this relation says, for picking a row's reason.
    pub(super) fn weight(self) -> f64 {
        match self {
            Relation::Implements | Relation::Extends => 1.5,
            Relation::Overrides => 1.2,
            Relation::Calls => 1.0,
            Relation::Uses => 0.7,
            Relation::Imports => 0.4,
        }
    }

    pub(super) fn of(kind: EdgeKind) -> Self {
        match kind {
            EdgeKind::Implements => Relation::Implements,
            EdgeKind::Extends => Relation::Extends,
            EdgeKind::Overrides => Relation::Overrides,
            EdgeKind::Calls => Relation::Calls,
            EdgeKind::Imports | EdgeKind::Exports => Relation::Imports,
            EdgeKind::References
            | EdgeKind::TypeOf
            | EdgeKind::Returns
            | EdgeKind::Instantiates
            | EdgeKind::Reads
            | EdgeKind::Writes
            | EdgeKind::Decorates
            | EdgeKind::Aliases
            | EdgeKind::Contains => Relation::Uses,
        }
    }
}

/// How much one edge of `kind` says about relatedness. Structural edges
/// (implements/extends) bind files tighter than a bare reference; imports
/// and exports are file-level plumbing.
pub(super) fn kind_weight(kind: EdgeKind) -> f64 {
    match kind {
        EdgeKind::Implements | EdgeKind::Extends => 1.5,
        EdgeKind::Overrides => 1.2,
        EdgeKind::Calls => 1.0,
        EdgeKind::Instantiates => 0.9,
        EdgeKind::TypeOf | EdgeKind::Returns => 0.8,
        EdgeKind::References => 0.7,
        EdgeKind::Reads | EdgeKind::Writes => 0.6,
        EdgeKind::Decorates | EdgeKind::Aliases => 0.5,
        EdgeKind::Imports => 0.4,
        EdgeKind::Exports => 0.3,
        EdgeKind::Contains => 0.0,
    }
}

/// Weight kept by a link through a generic name (see [`is_generic_name`]).
const GENERIC_LINK: f64 = 0.3;

/// A short all-lowercase name — `get`, `drop`, `state`, `run`. Edges to such
/// names are the ones name-based resolution most often guesses, and even when
/// right they tie a file to nothing in particular.
pub(super) fn is_generic_name(name: &str) -> bool {
    name.len() <= 5 && name.chars().all(|c| c.is_ascii_lowercase())
}

/// One-hop neighbourhood of `seeds`, keyed by neighbour path. Files in
/// `excluded` (explore's own ranked files) are never neighbours.
pub(super) fn gather(
    cg: &CodeGraph,
    seeds: &[&str],
    excluded: &HashSet<&str>,
) -> Result<HashMap<String, Neighbour>> {
    let mut neighbours: HashMap<String, Neighbour> = HashMap::new();
    for (rank, seed) in seeds.iter().enumerate() {
        crate::graph::cancel::check()?;
        for link in cg.get_cross_file_links(seed, LINKS_PER_SEED, LOCAL_NODES_PER_SEED)? {
            if excluded.contains(link.far_file.as_str()) {
                continue;
            }
            let entry = neighbours.entry(link.far_file.clone()).or_default();
            add_link(entry, rank, link);
        }
    }
    Ok(neighbours)
}

fn add_link(entry: &mut Neighbour, rank: usize, link: CrossFileLink) {
    let direction = if link.outgoing {
        Direction::Outbound
    } else {
        Direction::Inbound
    };
    // The symbol being used: the seed's for an inbound link, the neighbour's
    // for an outbound one.
    let used = match direction {
        Direction::Inbound => &link.local_name,
        Direction::Outbound => &link.far_name,
    };
    let weight = kind_weight(link.kind)
        * if is_generic_name(used) {
            GENERIC_LINK
        } else {
            1.0
        };
    entry.seed_ranks.insert(rank);
    match direction {
        Direction::Inbound => entry.inbound_weight += weight,
        Direction::Outbound => entry.outbound_weight += weight,
    }
    *entry
        .by_relation
        .entry((direction, Relation::of(link.kind)))
        .or_default()
        .entry(link.local_name.clone())
        .or_default() += 1;
    entry.seed_symbols.insert(link.local_name);
    let symbol = entry
        .symbols
        .entry(link.far_id)
        .or_insert_with(|| LinkedSymbol {
            name: link.far_name,
            kind: link.far_kind,
            start_line: link.far_start_line as usize,
            end_line: link.far_end_line as usize,
            weight: 0.0,
            inbound: false,
        });
    symbol.weight += weight;
    symbol.inbound |= direction == Direction::Inbound;
}

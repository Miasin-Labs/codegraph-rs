use std::collections::{HashMap, HashSet};

use super::super::format::ExploreOutputBudget;

pub(super) const MIN_CHARS: usize = 700;
const FILE_OVERHEAD: usize = 200;
const CLIFF_FRACTION: f64 = 0.15;
const CLIFF_MAX: f64 = 10.0;
const MAX_SHARE: f64 = 0.7;
const SPINE_WEIGHT_BOOST: f64 = 2.0;

pub(super) struct Candidate<'a> {
    pub path: &'a str,
    pub score: f64,
    pub worth: f64,
    pub spine: bool,
}

pub(super) struct Allocation {
    pub allowances: HashMap<String, usize>,
    pub cliffed: HashSet<String>,
    pub slot_limited: HashSet<String>,
    pub pool: usize,
}

pub(super) fn allocate(
    candidates: &[Candidate<'_>],
    budget: ExploreOutputBudget,
    max_files: usize,
) -> Allocation {
    let empty = Allocation {
        allowances: HashMap::new(),
        cliffed: HashSet::new(),
        slot_limited: HashSet::new(),
        pool: 0,
    };
    if candidates.is_empty() {
        return empty;
    }
    let weight = |candidate: &Candidate<'_>| {
        let value = candidate.score.max(0.0)
            * candidate.worth.clamp(0.0, 1.0)
            * if candidate.spine {
                SPINE_WEIGHT_BOOST
            } else {
                1.0
            };
        if value.is_finite() { value } else { 0.0 }
    };
    let weights = candidates
        .iter()
        .map(|candidate| (candidate.path, weight(candidate)))
        .collect::<HashMap<_, _>>();
    let top = weights.values().copied().fold(0.0f64, f64::max);
    if top <= 0.0 {
        return empty;
    }
    let cliff_at = (top * CLIFF_FRACTION).min(CLIFF_MAX);
    let mut cliffed = HashSet::new();
    let mut admitted = candidates
        .iter()
        .filter(|candidate| {
            let keep = candidate.spine || weights[candidate.path] >= cliff_at;
            if !keep {
                cliffed.insert(candidate.path.to_string());
            }
            keep
        })
        .collect::<Vec<_>>();
    if admitted.is_empty() {
        admitted.push(&candidates[0]);
        cliffed.remove(candidates[0].path);
    }
    let slot_limited = admitted
        .iter()
        .skip(max_files)
        .map(|candidate| candidate.path.to_string())
        .collect();
    admitted.truncate(max_files);
    let affordable = (budget.max_output_chars / (MIN_CHARS + FILE_OVERHEAD)).max(1);
    if admitted.len() > affordable {
        let mut by_weight = admitted.clone();
        by_weight.sort_by(|left, right| weights[right.path].total_cmp(&weights[left.path]));
        let mut keep = by_weight
            .into_iter()
            .take(affordable)
            .map(|candidate| candidate.path)
            .collect::<HashSet<_>>();
        keep.extend(
            admitted
                .iter()
                .filter(|candidate| candidate.spine)
                .map(|candidate| candidate.path),
        );
        admitted.retain(|candidate| {
            let retained = keep.contains(candidate.path);
            if !retained {
                cliffed.insert(candidate.path.to_string());
            }
            retained
        });
    }
    let pool = budget
        .max_output_chars
        .saturating_sub(FILE_OVERHEAD * admitted.len());
    let total = admitted
        .iter()
        .map(|candidate| weights[candidate.path])
        .sum::<f64>();
    if admitted.is_empty() || total <= 0.0 {
        return Allocation {
            allowances: HashMap::new(),
            cliffed,
            slot_limited,
            pool,
        };
    }
    let admitted_len = admitted.len();
    let floors = pool.min(MIN_CHARS * admitted_len);
    let remainder = pool - floors;
    let ceiling = (budget.max_output_chars as f64 * MAX_SHARE).round() as usize;
    let allowances = admitted
        .into_iter()
        .map(|candidate| {
            let share = floors / admitted_len
                + (remainder as f64 * weights[candidate.path] / total).floor() as usize;
            (candidate.path.to_string(), share.min(ceiling))
        })
        .collect();
    Allocation {
        allowances,
        cliffed,
        slot_limited,
        pool,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::tools::get_explore_output_budget;

    fn candidate(path: &str, score: f64) -> Candidate<'_> {
        Candidate {
            path,
            score,
            worth: 1.0,
            spine: false,
        }
    }

    #[test]
    fn score_ratio_controls_the_split() {
        let budget = get_explore_output_budget(1_000);
        let wide = allocate(&[candidate("a", 40.0), candidate("b", 10.0)], budget, 8);
        let narrow = allocate(&[candidate("a", 22.0), candidate("b", 20.0)], budget, 8);
        assert!(
            wide.allowances["a"] / wide.allowances["b"]
                > narrow.allowances["a"] / narrow.allowances["b"]
        );
    }

    #[test]
    fn relative_cliff_hands_the_slot_to_the_next_file() {
        let allocation = allocate(
            &[
                candidate("a", 90.0),
                candidate("noise", 2.0),
                candidate("b", 40.0),
            ],
            get_explore_output_budget(1_000),
            2,
        );
        assert_eq!(
            allocation
                .allowances
                .keys()
                .cloned()
                .collect::<HashSet<_>>(),
            HashSet::from(["a".into(), "b".into()])
        );
        assert!(allocation.cliffed.contains("noise"));
        assert!(!allocation.slot_limited.contains("noise"));
    }

    #[test]
    fn diffuse_candidates_receive_a_usable_floor_without_starvation() {
        let names = (0..8).map(|index| format!("f{index}")).collect::<Vec<_>>();
        let candidates = names
            .iter()
            .enumerate()
            .map(|(index, name)| candidate(name, 30.0 - index as f64))
            .collect::<Vec<_>>();
        let allocation = allocate(&candidates, get_explore_output_budget(1_000), 8);
        assert_eq!(allocation.allowances.len(), 8);
        assert!(
            allocation
                .allowances
                .values()
                .all(|value| *value >= MIN_CHARS)
        );
    }

    #[test]
    fn spine_survives_the_cliff_and_earns_more_than_a_peer() {
        let candidates = [
            candidate("peer", 20.0),
            Candidate {
                path: "spine",
                score: 20.0,
                worth: 1.0,
                spine: true,
            },
        ];
        let allocation = allocate(&candidates, get_explore_output_budget(1_000), 8);
        assert!(allocation.allowances["spine"] > allocation.allowances["peer"]);
    }

    #[test]
    fn reservations_never_exceed_the_pool() {
        let names = (0..30).map(|index| format!("f{index}")).collect::<Vec<_>>();
        let candidates = names
            .iter()
            .enumerate()
            .map(|(index, name)| candidate(name, 100.0 - index as f64))
            .collect::<Vec<_>>();
        for tier in [10, 100, 500, 1_000, 10_000, 60_000] {
            let allocation = allocate(&candidates, get_explore_output_budget(tier), 8);
            assert!(allocation.allowances.values().sum::<usize>() <= allocation.pool);
        }
    }
}

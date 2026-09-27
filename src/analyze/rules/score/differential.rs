//! Differential scoring of vulnerable/fixed pairs (RustSec) —
//! `tools/bugbench/differential.py`, number for number.
//!
//! The fix region of a pair is its `bad` ground-truth rows under `vuln/`
//! (`bad_candidate` rows only when the pair has no `bad` row): a function
//! row covers its function, a line row its hunk ± `slack` lines. Each vuln
//! finding is then
//!
//! - `tp`: in the fix region and absent from the same function in `fixed/`;
//! - `not_discriminating`: in the fix region, but `fixed/` has it too;
//! - `vuln_only_elsewhere`: gone in `fixed/`, but outside the fix region;
//! - `background`: outside the region and present in both versions.
//!
//! A finding persists across the fix when `fixed/` has one with the same
//! rule, file and function (or, outside any function, the same message
//! modulo digits). `introduced` counts fixed-only findings.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

use super::gt::{FileIndex, GtRow};
use super::{ScoredFinding, ratio};

pub(crate) const OUTCOMES: [&str; 4] = [
    "tp",
    "not_discriminating",
    "vuln_only_elsewhere",
    "background",
];

static DIGITS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d+").expect("valid regex"));

/// One pair to score.
pub(crate) struct PairInput<'a> {
    pub advisory: String,
    pub categories: Vec<String>,
    pub localization: String,
    pub vuln: Vec<&'a ScoredFinding>,
    pub fixed: Vec<&'a ScoredFinding>,
    pub region: Vec<GtRow>,
}

/// What one vuln finding scored.
#[derive(Debug, Clone)]
pub(crate) struct DiffVerdict<'a> {
    pub finding: &'a ScoredFinding,
    pub outcome: &'static str,
}

fn persist_key(finding: &ScoredFinding) -> (String, String, String) {
    let place = match finding.function.as_deref() {
        Some(function) if !function.is_empty() => function.to_string(),
        _ => DIGITS.replace_all(&finding.message, "#").into_owned(),
    };
    (finding.rule_id.clone(), finding.local_file.clone(), place)
}

/// A pair's fix region among its advisory's rows.
pub(crate) fn region_rows(advisory: &str, rows: &[&GtRow]) -> Vec<GtRow> {
    let prefix = format!("{advisory}/vuln/");
    let vuln: Vec<&GtRow> = rows
        .iter()
        .copied()
        .filter(|row| row.file.starts_with(&prefix))
        .collect();
    let bad: Vec<GtRow> = vuln
        .iter()
        .filter(|row| row.label == "bad")
        .map(|row| (*row).clone())
        .collect();
    if !bad.is_empty() {
        return bad;
    }
    vuln.iter()
        .filter(|row| row.label == "bad_candidate")
        .map(|row| (*row).clone())
        .collect()
}

type Counts = BTreeMap<&'static str, usize>;

fn score_pair<'a>(
    pair: &PairInput<'a>,
    slack: i64,
) -> (Vec<DiffVerdict<'a>>, BTreeMap<String, usize>) {
    let index = FileIndex::new(&pair.region);
    let fixed_keys: HashSet<_> = pair.fixed.iter().map(|f| persist_key(f)).collect();
    let vuln_keys: HashSet<_> = pair.vuln.iter().map(|f| persist_key(f)).collect();
    let verdicts = pair
        .vuln
        .iter()
        .map(|finding| {
            let in_region = !index
                .at(&finding.file, finding.line, slack, None)
                .is_empty();
            let persists = fixed_keys.contains(&persist_key(finding));
            let outcome = match (in_region, persists) {
                (true, true) => "not_discriminating",
                (true, false) => "tp",
                (false, true) => "background",
                (false, false) => "vuln_only_elsewhere",
            };
            DiffVerdict { finding, outcome }
        })
        .collect();
    let mut introduced: BTreeMap<String, usize> = BTreeMap::new();
    for finding in &pair.fixed {
        if !vuln_keys.contains(&persist_key(finding)) {
            *introduced.entry(finding.rule_id.clone()).or_default() += 1;
        }
    }
    (verdicts, introduced)
}

/// One rule's (or the overall) numbers.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct DiffRow {
    pub tp: usize,
    pub not_discriminating: usize,
    pub vuln_only_elsewhere: usize,
    pub background: usize,
    pub introduced: usize,
    pub vuln_findings: usize,
    pub pairs_detected: usize,
    pub recall_pairs: Option<f64>,
    pub precision_differential: Option<f64>,
    pub discriminating_share_in_region: Option<f64>,
}

/// One pair's numbers.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct PairRow {
    pub advisory: String,
    pub categories: Vec<String>,
    pub localization: String,
    pub region_rows: usize,
    pub vuln_findings: usize,
    pub fixed_findings: usize,
    pub tp: usize,
    pub not_discriminating: usize,
    pub vuln_only_elsewhere: usize,
    pub background: usize,
    pub introduced: usize,
}

/// `score_pairs`' metrics (its `verdicts` are returned apart).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct DiffMetrics {
    pub pairs: usize,
    pub scoreable_pairs: usize,
    pub slack: i64,
    pub overall: DiffRow,
    pub per_rule: BTreeMap<String, DiffRow>,
    pub per_category: BTreeMap<String, BTreeMap<String, serde_json::Value>>,
    pub per_pair: Vec<PairRow>,
}

fn diff_row(counts: &Counts, pairs_hit: usize, scoreable: usize) -> DiffRow {
    let get = |key: &str| counts.get(key).copied().unwrap_or(0);
    let differential = get("tp") + get("vuln_only_elsewhere");
    let region = get("tp") + get("not_discriminating");
    DiffRow {
        tp: get("tp"),
        not_discriminating: get("not_discriminating"),
        vuln_only_elsewhere: get("vuln_only_elsewhere"),
        background: get("background"),
        introduced: get("introduced"),
        vuln_findings: OUTCOMES.iter().map(|o| get(o)).sum(),
        pairs_detected: pairs_hit,
        recall_pairs: ratio(pairs_hit, scoreable),
        precision_differential: ratio(get("tp"), differential),
        discriminating_share_in_region: ratio(get("tp"), region),
    }
}

/// Metrics over every pair.
pub(crate) fn score_pairs<'a>(
    pairs: &[PairInput<'a>],
    slack: i64,
) -> (DiffMetrics, Vec<DiffVerdict<'a>>) {
    let mut per_rule: BTreeMap<String, Counts> = BTreeMap::new();
    let mut per_rule_pairs: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut per_cat: BTreeMap<String, BTreeMap<&'static str, usize>> = BTreeMap::new();
    let mut per_pair = Vec::new();
    let mut all_verdicts = Vec::new();
    let mut detected: BTreeSet<String> = BTreeSet::new();
    let scoreable = pairs.iter().filter(|p| !p.region.is_empty()).count();
    for pair in pairs {
        let (verdicts, introduced) = score_pair(pair, slack);
        let mut outcomes: Counts = BTreeMap::new();
        for verdict in &verdicts {
            *outcomes.entry(verdict.outcome).or_default() += 1;
            *per_rule
                .entry(verdict.finding.rule_id.clone())
                .or_default()
                .entry(verdict.outcome)
                .or_default() += 1;
            if verdict.outcome == "tp" {
                per_rule_pairs
                    .entry(verdict.finding.rule_id.clone())
                    .or_default()
                    .insert(pair.advisory.clone());
                detected.insert(pair.advisory.clone());
            }
        }
        for (rule, n) in &introduced {
            *per_rule
                .entry(rule.clone())
                .or_default()
                .entry("introduced")
                .or_default() += n;
        }
        let get = |key: &str| outcomes.get(key).copied().unwrap_or(0);
        for category in &pair.categories {
            let row = per_cat.entry(category.clone()).or_default();
            *row.entry("pairs").or_default() += 1;
            *row.entry("scoreable").or_default() += usize::from(!pair.region.is_empty());
            *row.entry("detected").or_default() += usize::from(detected.contains(&pair.advisory));
            *row.entry("region_findings").or_default() += get("tp") + get("not_discriminating");
        }
        all_verdicts.extend(verdicts);
        per_pair.push(PairRow {
            advisory: pair.advisory.clone(),
            categories: pair.categories.clone(),
            localization: pair.localization.clone(),
            region_rows: pair.region.len(),
            vuln_findings: pair.vuln.len(),
            fixed_findings: pair.fixed.len(),
            tp: get("tp"),
            not_discriminating: get("not_discriminating"),
            vuln_only_elsewhere: get("vuln_only_elsewhere"),
            background: get("background"),
            introduced: introduced.values().sum(),
        });
    }

    let mut overall: Counts = BTreeMap::new();
    for counts in per_rule.values() {
        for (key, n) in counts {
            *overall.entry(key).or_default() += n;
        }
    }
    let metrics = DiffMetrics {
        pairs: pairs.len(),
        scoreable_pairs: scoreable,
        slack,
        overall: diff_row(&overall, detected.len(), scoreable),
        per_rule: per_rule
            .iter()
            .map(|(rule, counts)| {
                let hit = per_rule_pairs.get(rule).map_or(0, BTreeSet::len);
                (rule.clone(), diff_row(counts, hit, scoreable))
            })
            .collect(),
        per_category: per_cat
            .into_iter()
            .map(|(category, counts)| {
                let get = |key: &str| counts.get(key).copied().unwrap_or(0);
                let recall = ratio(get("detected"), get("scoreable"));
                let mut row: BTreeMap<String, serde_json::Value> = counts
                    .iter()
                    .map(|(key, n)| (key.to_string(), serde_json::Value::from(*n)))
                    .collect();
                row.insert("recall".to_string(), serde_json::Value::from(recall));
                (category, row)
            })
            .collect(),
        per_pair,
    };
    (metrics, all_verdicts)
}

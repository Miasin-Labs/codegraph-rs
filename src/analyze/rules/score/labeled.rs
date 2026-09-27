//! Scoring against labeled functions, files and lines (Juliet, OWASP, the
//! web apps) — `tools/bugbench/labeled.py`, number for number.
//!
//! A finding inside a `bad` row is a TP (credited to that row's class);
//! inside only a `good` row, an FP; anywhere else, unlabeled (reported
//! apart: on the web apps unlabeled code is not known-clean). Recall counts
//! the `bad` rows of the scored files hit by at least one finding.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::Serialize;

use super::gt::{BAD, FileIndex, GOOD, GtRow};
use super::{ScoredFinding, ratio};

/// A finding within this many lines of a Juliet flaw line.
const FLAW_SLACK: i64 = 2;

/// What one finding scored.
#[derive(Debug, Clone)]
pub(crate) struct Verdict<'a> {
    pub finding: &'a ScoredFinding,
    /// `tp`, `fp` or `unlabeled`.
    pub outcome: &'static str,
    pub rows: Vec<&'a GtRow>,
    pub flaw_hit: bool,
}

#[derive(Default)]
struct RuleStats {
    findings: usize,
    tp: usize,
    fp: usize,
    unlabeled: usize,
    flaw_hits: usize,
    bad_hit: HashSet<usize>,
    good_hit: HashSet<usize>,
}

impl RuleStats {
    fn add(&mut self, verdict: &Verdict) {
        self.findings += 1;
        match verdict.outcome {
            "tp" => {
                self.tp += 1;
                self.bad_hit.extend(verdict.rows.iter().map(|r| r.id));
            }
            "fp" => {
                self.fp += 1;
                self.good_hit.extend(verdict.rows.iter().map(|r| r.id));
            }
            _ => self.unlabeled += 1,
        }
        self.flaw_hits += usize::from(verdict.flaw_hit);
    }

    fn row(&self, bad_rows: usize, good_rows: usize) -> RuleRow {
        RuleRow {
            findings: self.findings,
            tp: self.tp,
            fp: self.fp,
            unlabeled: self.unlabeled,
            flaw_line_hits: self.flaw_hits,
            precision: ratio(self.tp, self.tp + self.fp),
            bad_hit: self.bad_hit.len(),
            recall: ratio(self.bad_hit.len(), bad_rows),
            good_flagged: self.good_hit.len(),
            good_flag_rate: ratio(self.good_hit.len(), good_rows),
        }
    }
}

/// One rule's (or the overall) numbers.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct RuleRow {
    pub findings: usize,
    pub tp: usize,
    pub fp: usize,
    pub unlabeled: usize,
    pub flaw_line_hits: usize,
    pub precision: Option<f64>,
    pub bad_hit: usize,
    pub recall: Option<f64>,
    pub good_flagged: usize,
    pub good_flag_rate: Option<f64>,
}

/// Row counts by label.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct RowCounts {
    pub bad: usize,
    pub good: usize,
    pub other: usize,
}

/// One class's (CWE or category) numbers.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ClassRow {
    pub bad: usize,
    pub good: usize,
    pub bad_hit: usize,
    pub recall: Option<f64>,
    pub good_flagged: usize,
    pub precision: Option<f64>,
    pub tp_by_rule: BTreeMap<String, usize>,
    pub fp_by_rule: BTreeMap<String, usize>,
}

/// `score_labeled`'s metrics (its `verdicts` are returned apart).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct LabeledMetrics {
    pub rows: RowCounts,
    /// The precision of a detector that fires in labeled rows at random.
    pub base_rate: Option<f64>,
    pub overall: RuleRow,
    pub per_rule: BTreeMap<String, RuleRow>,
    pub per_class: BTreeMap<String, ClassRow>,
}

fn classify<'a>(finding: &'a ScoredFinding, index: &FileIndex<'a>, slack: i64) -> Verdict<'a> {
    let bad = index.at(&finding.file, finding.line, slack, Some(&[BAD]));
    if !bad.is_empty() {
        let flaw_hit = bad.iter().any(|row| {
            row.flaw_lines
                .iter()
                .any(|flaw| (finding.line - flaw).abs() <= FLAW_SLACK)
        });
        return Verdict {
            finding,
            outcome: "tp",
            rows: bad,
            flaw_hit,
        };
    }
    let good = index.at(&finding.file, finding.line, slack, Some(&[GOOD]));
    if !good.is_empty() {
        return Verdict {
            finding,
            outcome: "fp",
            rows: good,
            flaw_hit: false,
        };
    }
    Verdict {
        finding,
        outcome: "unlabeled",
        rows: Vec::new(),
        flaw_hit: false,
    }
}

/// Per-rule and per-class metrics of `findings` over `rows` (the rows of
/// the scored files).
pub(crate) fn score_labeled<'a>(
    findings: &'a [ScoredFinding],
    rows: &'a [GtRow],
    slack: i64,
) -> (LabeledMetrics, Vec<Verdict<'a>>) {
    let index = FileIndex::new(rows);
    let bad_rows = rows.iter().filter(|r| r.label == BAD).count();
    let good_rows = rows.iter().filter(|r| r.label == GOOD).count();
    let verdicts: Vec<Verdict> = findings
        .iter()
        .map(|finding| classify(finding, &index, slack))
        .collect();

    let mut per_rule: BTreeMap<String, RuleStats> = BTreeMap::new();
    let mut overall = RuleStats::default();
    for verdict in &verdicts {
        per_rule
            .entry(verdict.finding.rule_id.clone())
            .or_default()
            .add(verdict);
        overall.add(verdict);
    }

    let mut bad_by_class: BTreeMap<&str, usize> = BTreeMap::new();
    let mut good_by_class: BTreeMap<&str, usize> = BTreeMap::new();
    for row in rows {
        if row.label == BAD {
            *bad_by_class.entry(row.klass()).or_default() += 1;
        } else if row.label == GOOD {
            *good_by_class.entry(row.klass()).or_default() += 1;
        }
    }
    let classes: BTreeSet<&str> = bad_by_class
        .keys()
        .chain(good_by_class.keys())
        .copied()
        .collect();
    let mut per_class = BTreeMap::new();
    for klass in classes {
        let hit_bad = overall
            .bad_hit
            .iter()
            .filter(|id| rows[**id].klass() == klass)
            .count();
        let hit_good = overall
            .good_hit
            .iter()
            .filter(|id| rows[**id].klass() == klass)
            .count();
        let by_rule = |outcome: &str| {
            let mut counts: BTreeMap<String, usize> = BTreeMap::new();
            for verdict in &verdicts {
                if verdict.outcome == outcome && verdict.rows.iter().any(|r| r.klass() == klass) {
                    *counts.entry(verdict.finding.rule_id.clone()).or_default() += 1;
                }
            }
            counts
        };
        let tp_by_rule = by_rule("tp");
        let fp_by_rule = by_rule("fp");
        let tp: usize = tp_by_rule.values().sum();
        let fp: usize = fp_by_rule.values().sum();
        let bad = bad_by_class.get(klass).copied().unwrap_or(0);
        per_class.insert(
            klass.to_string(),
            ClassRow {
                bad,
                good: good_by_class.get(klass).copied().unwrap_or(0),
                bad_hit: hit_bad,
                recall: ratio(hit_bad, bad),
                good_flagged: hit_good,
                precision: ratio(tp, tp + fp),
                tp_by_rule,
                fp_by_rule,
            },
        );
    }

    let metrics = LabeledMetrics {
        rows: RowCounts {
            bad: bad_rows,
            good: good_rows,
            other: rows.len() - bad_rows - good_rows,
        },
        base_rate: ratio(bad_rows, bad_rows + good_rows),
        overall: overall.row(bad_rows, good_rows),
        per_rule: per_rule
            .iter()
            .map(|(rule, stats)| (rule.clone(), stats.row(bad_rows, good_rows)))
            .collect(),
        per_class,
    };
    (metrics, verdicts)
}

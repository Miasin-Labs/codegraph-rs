"""Scoring against labeled functions/files/lines (Juliet, OWASP, web apps).

A finding inside a `bad` row is a TP (credited to that row's CWE); inside
only a `good` row, an FP; anywhere else, unlabeled (reported apart: on the
web apps unlabeled code is not known-clean). Recall counts `bad` rows in
the scored files hit by at least one finding.
"""

from __future__ import annotations

from collections import Counter, defaultdict
from dataclasses import dataclass, field
from typing import Any

from findings import Finding
from gt import BAD, GOOD, FileIndex, GtRow

FLAW_SLACK = 2  # a finding within this many lines of a Juliet flaw line


@dataclass
class Verdict:
    finding: Finding
    outcome: str  # tp | fp | unlabeled
    rows: list[GtRow]
    flaw_hit: bool = False


@dataclass
class RuleStats:
    findings: int = 0
    tp: int = 0
    fp: int = 0
    unlabeled: int = 0
    flaw_hits: int = 0
    bad_hit: set[int] = field(default_factory=set)
    good_hit: set[int] = field(default_factory=set)

    def add(self, verdict: Verdict) -> None:
        self.findings += 1
        if verdict.outcome == "tp":
            self.tp += 1
            self.bad_hit.update(r.id for r in verdict.rows)
        elif verdict.outcome == "fp":
            self.fp += 1
            self.good_hit.update(r.id for r in verdict.rows)
        else:
            self.unlabeled += 1
        self.flaw_hits += verdict.flaw_hit


def classify(finding: Finding, index: FileIndex, slack: int) -> Verdict:
    bad = index.at(finding.file, finding.line, slack, {BAD})
    if bad:
        flaw = any(abs(finding.line - fl) <= FLAW_SLACK for r in bad for fl in r.flaw_lines)
        return Verdict(finding, "tp", bad, flaw)
    good = index.at(finding.file, finding.line, slack, {GOOD})
    if good:
        return Verdict(finding, "fp", good)
    return Verdict(finding, "unlabeled", [])


def ratio(num: int, den: int) -> float | None:
    return round(num / den, 4) if den else None


def score_labeled(findings: list[Finding], rows: list[GtRow], slack: int) -> dict[str, Any]:
    """Per-rule and per-class metrics over the rows of the scored files."""
    index = FileIndex(rows)
    bad_rows = [r for r in rows if r.label == BAD]
    good_rows = [r for r in rows if r.label == GOOD]
    verdicts = [classify(f, index, slack) for f in findings]

    per_rule: dict[str, RuleStats] = defaultdict(RuleStats)
    overall = RuleStats()
    for verdict in verdicts:
        per_rule[verdict.finding.rule_id].add(verdict)
        overall.add(verdict)

    def rule_row(stats: RuleStats) -> dict[str, Any]:
        return {
            "findings": stats.findings, "tp": stats.tp, "fp": stats.fp,
            "unlabeled": stats.unlabeled, "flaw_line_hits": stats.flaw_hits,
            "precision": ratio(stats.tp, stats.tp + stats.fp),
            "bad_hit": len(stats.bad_hit), "recall": ratio(len(stats.bad_hit), len(bad_rows)),
            "good_flagged": len(stats.good_hit),
            "good_flag_rate": ratio(len(stats.good_hit), len(good_rows)),
        }

    per_class: dict[str, dict[str, Any]] = {}
    bad_by_class = Counter(r.klass for r in bad_rows)
    good_by_class = Counter(r.klass for r in good_rows)
    for klass in sorted(set(bad_by_class) | set(good_by_class)):
        hit_bad = {rid for rid in overall.bad_hit if rows[rid].klass == klass}
        hit_good = {rid for rid in overall.good_hit if rows[rid].klass == klass}
        tp_rules = Counter(v.finding.rule_id for v in verdicts
                           if v.outcome == "tp" and any(r.klass == klass for r in v.rows))
        fp_rules = Counter(v.finding.rule_id for v in verdicts
                           if v.outcome == "fp" and any(r.klass == klass for r in v.rows))
        per_class[klass] = {
            "bad": bad_by_class[klass], "good": good_by_class[klass],
            "bad_hit": len(hit_bad), "recall": ratio(len(hit_bad), bad_by_class[klass]),
            "good_flagged": len(hit_good),
            "precision": ratio(sum(tp_rules.values()), sum(tp_rules.values()) + sum(fp_rules.values())),
            "tp_by_rule": dict(tp_rules), "fp_by_rule": dict(fp_rules),
        }

    return {
        "rows": {"bad": len(bad_rows), "good": len(good_rows), "other": len(rows) - len(bad_rows) - len(good_rows)},
        # the precision of a detector that fires in labeled rows at random
        "base_rate": ratio(len(bad_rows), len(bad_rows) + len(good_rows)),
        "overall": rule_row(overall),
        "per_rule": {rule: rule_row(s) for rule, s in sorted(per_rule.items())},
        "per_class": per_class,
        "verdicts": verdicts,
    }

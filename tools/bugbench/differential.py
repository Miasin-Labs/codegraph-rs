"""Differential scoring of rustsec vuln/fixed pairs.

The fix region of a pair is its `bad` ground-truth rows under `vuln/`
(fix-commit, name-match or Rudra rows; `bad_candidate` rows only when the
pair has no `bad` row): a function row covers its function, a line row its
hunk +- `slack` lines. Each vuln finding is then:

- `tp`: in the fix region and absent from the same function in `fixed/`;
- `not_discriminating`: in the fix region, but `fixed/` has it too;
- `vuln_only_elsewhere`: gone in `fixed/`, but outside the fix region;
- `background`: outside the region and present in both versions.

A finding persists across the fix when `fixed/` has one with the same rule,
file and function (or, outside any function, the same message modulo
digits). `introduced` counts fixed-only findings.
"""

from __future__ import annotations

import re
from collections import Counter, defaultdict
from dataclasses import dataclass
from typing import Any

from findings import Finding
from gt import FileIndex, GtRow

OUTCOMES = ("tp", "not_discriminating", "vuln_only_elsewhere", "background")
DIGITS = re.compile(r"\d+")


@dataclass
class PairInput:
    advisory: str
    categories: list[str]
    localization: str
    vuln: list[Finding]
    fixed: list[Finding]
    region: list[GtRow]


@dataclass
class DiffVerdict:
    finding: Finding
    outcome: str
    advisory: str


def persist_key(f: Finding) -> tuple[str, str, str]:
    where = f.function if f.function else DIGITS.sub("#", f.message)
    return (f.rule_id, f.local_file, where)


def region_rows(advisory: str, rows: list[GtRow]) -> list[GtRow]:
    vuln = [r for r in rows if r.file.startswith(f"{advisory}/vuln/")]
    bad = [r for r in vuln if r.label == "bad"]
    return bad or [r for r in vuln if r.label == "bad_candidate"]


def score_pair(pair: PairInput, slack: int) -> tuple[list[DiffVerdict], Counter[str]]:
    index = FileIndex(pair.region)
    fixed_keys = {persist_key(f) for f in pair.fixed}
    vuln_keys = {persist_key(f) for f in pair.vuln}
    verdicts: list[DiffVerdict] = []
    for f in pair.vuln:
        in_region = bool(index.at(f.file, f.line, slack))
        persists = persist_key(f) in fixed_keys
        outcome = (("not_discriminating" if persists else "tp") if in_region
                   else ("background" if persists else "vuln_only_elsewhere"))
        verdicts.append(DiffVerdict(f, outcome, pair.advisory))
    introduced = Counter(f.rule_id for f in pair.fixed if persist_key(f) not in vuln_keys)
    return verdicts, introduced


def ratio(num: int, den: int) -> float | None:
    return round(num / den, 4) if den else None


def score_pairs(pairs: list[PairInput], slack: int) -> dict[str, Any]:
    per_rule: dict[str, Counter[str]] = defaultdict(Counter)
    per_rule_pairs: dict[str, set[str]] = defaultdict(set)
    per_cat: dict[str, Counter[str]] = defaultdict(Counter)
    per_pair: list[dict[str, Any]] = []
    all_verdicts: list[DiffVerdict] = []
    detected: set[str] = set()
    scoreable = [p for p in pairs if p.region]
    for pair in pairs:
        verdicts, introduced = score_pair(pair, slack)
        all_verdicts.extend(verdicts)
        outcomes = Counter(v.outcome for v in verdicts)
        for v in verdicts:
            per_rule[v.finding.rule_id][v.outcome] += 1
            if v.outcome == "tp":
                per_rule_pairs[v.finding.rule_id].add(pair.advisory)
                detected.add(pair.advisory)
        for rule, n in introduced.items():
            per_rule[rule]["introduced"] += n
        for cat in pair.categories:
            per_cat[cat]["pairs"] += 1
            per_cat[cat]["scoreable"] += bool(pair.region)
            per_cat[cat]["detected"] += pair.advisory in detected
            per_cat[cat]["region_findings"] += outcomes["tp"] + outcomes["not_discriminating"]
        per_pair.append({
            "advisory": pair.advisory, "categories": pair.categories,
            "localization": pair.localization, "region_rows": len(pair.region),
            "vuln_findings": len(pair.vuln), "fixed_findings": len(pair.fixed),
            **{o: outcomes[o] for o in OUTCOMES}, "introduced": sum(introduced.values()),
        })

    def rule_row(c: Counter[str], rule: str | None) -> dict[str, Any]:
        diff = c["tp"] + c["vuln_only_elsewhere"]
        region = c["tp"] + c["not_discriminating"]
        pairs_hit = len(per_rule_pairs[rule]) if rule else len(detected)
        return {
            **{o: c[o] for o in OUTCOMES}, "introduced": c["introduced"],
            "vuln_findings": sum(c[o] for o in OUTCOMES),
            "pairs_detected": pairs_hit,
            "recall_pairs": ratio(pairs_hit, len(scoreable)),
            "precision_differential": ratio(c["tp"], diff),
            "discriminating_share_in_region": ratio(c["tp"], region),
        }

    overall: Counter[str] = Counter()
    for c in per_rule.values():
        overall.update(c)
    return {
        "pairs": len(pairs), "scoreable_pairs": len(scoreable), "slack": slack,
        "overall": rule_row(overall, None),
        "per_rule": {r: rule_row(c, r) for r, c in sorted(per_rule.items())},
        "per_category": {k: dict(v) | {"recall": ratio(v["detected"], v["scoreable"])}
                         for k, v in sorted(per_cat.items())},
        "per_pair": per_pair,
        "verdicts": all_verdicts,
    }

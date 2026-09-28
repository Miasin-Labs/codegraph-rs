#!/usr/bin/env python3
"""Compare two stored detector outputs alone, as a union, and as an
intersection (the high-confidence tier), on a labeled corpus.

    combine.py <corpus> --a rules --b codeql [--merged combined] [--bench DIR]

Run `run.py` first with both labels (and optionally the in-product merge,
`analyze codeql --builtin`). Two views:

* **findings** — bugbench's own scoring (`labeled.py`): a finding inside a
  bad row is a TP, inside a good row an FP; recall = bad rows hit. The
  intersection keeps the findings of either side that the other side
  corroborates (same file and function, a shared CWE).
* **test cases** — the OWASP Benchmark scorecard: a labeled row (a test
  case) is flagged when a finding lands in it; `cwe` also requires the
  finding's rule to carry the row's CWE. Reports TPR, FPR and
  TPR − FPR (Youden's J, the Benchmark score), overall and per class.

A rule's CWEs come from the built-in rules' `tags` and, for `codeql::`
rules, from the SARIF files CodeQL left in the kept work dirs (`run.py
--keep`), or `--cwe-map FILE` (JSON: rule → [CWE-N…]).
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any

import yaml

from config import REPO_ROOT, Paths, default_bench
from findings import Finding, load_unit
from gt import BAD, GOOD, FileIndex, GtRow, load_rows
from labeled import score_labeled
from report import pct, table
from runner import safe_name
from units import enumerate_units

CWE_TAG = re.compile(r"(?i)^(?:external/cwe/)?cwe-0*(\d+)$")


def cwes_of(tags: list[str]) -> set[str]:
    return {f"CWE-{m.group(1)}" for t in tags if (m := CWE_TAG.match(str(t)))}


def rule_cwes(paths: Paths, corpus: str, units: list[str], extra: Path | None) -> dict[str, set[str]]:
    """rule id (as findings name it) → CWEs."""
    out: dict[str, set[str]] = {}
    for path in sorted((REPO_ROOT / "src/analyze/rules/builtin").glob("*.yaml")):
        for doc in yaml.safe_load_all(path.read_text()):
            for rule in doc if isinstance(doc, list) else [doc]:
                if isinstance(rule, dict) and "id" in rule:
                    out[str(rule["id"])] = cwes_of(rule.get("tags") or [])
    for unit in units:
        sarif_dir = paths.work / corpus / safe_name(unit) / ".codegraph" / "codeql" / "sarif"
        for sarif in sorted(sarif_dir.glob("*.sarif")):
            log = json.loads(sarif.read_text())
            for run in log.get("runs", []):
                components = [run["tool"]["driver"], *run["tool"].get("extensions", [])]
                for component in components:
                    for rule in component.get("rules", []):
                        tags = (rule.get("properties") or {}).get("tags") or []
                        out[f"codeql::{rule['id']}"] = cwes_of(tags)
    if extra:
        for rule, cwes in json.loads(extra.read_text()).items():
            out[rule] = set(cwes)
    return out


def row_of(index: FileIndex, f: Finding, slack: int) -> GtRow | None:
    rows = index.at(f.file, f.line, slack, {BAD}) or index.at(f.file, f.line, slack, {GOOD})
    return rows[0] if rows else None


def corroborated(mine: list[Finding], theirs: list[Finding], cwes: dict[str, set[str]]) -> list[Finding]:
    """`mine` that `theirs` agrees with: same file and function (or within 3
    lines when neither has one) and a shared CWE."""
    by_file: dict[str, list[Finding]] = defaultdict(list)
    for f in theirs:
        by_file[f.file].append(f)
    kept = []
    for f in mine:
        ours = cwes.get(f.rule, set())
        for g in by_file.get(f.file, []):
            same = (f.function == g.function) if (f.function or g.function) else abs(f.line - g.line) <= 3
            if same and ours & cwes.get(g.rule, set()):
                kept.append(f)
                break
    return kept


def finding_view(findings: list[Finding], rows: list[GtRow], slack: int) -> dict[str, Any]:
    m = score_labeled(findings, rows, slack)
    o = m["overall"]
    return {"findings": o["findings"], "tp": o["tp"], "fp": o["fp"], "unlabeled": o["unlabeled"],
            "precision": o["precision"], "recall": o["recall"], "bad_hit": o["bad_hit"],
            "good_flagged": o["good_flagged"]}


def flagged(findings: list[Finding], index: FileIndex, cwes: dict[str, set[str]], slack: int,
            match_cwe: bool) -> set[int]:
    out = set()
    for f in findings:
        for row in index.at(f.file, f.line, slack):
            if row.label not in (BAD, GOOD):
                continue
            if match_cwe and (row.cwe or "") not in cwes.get(f.rule, set()):
                continue
            out.add(row.id)
    return out


def case_view(hit: set[int], rows: list[GtRow]) -> dict[str, Any]:
    bad = [r for r in rows if r.label == BAD]
    good = [r for r in rows if r.label == GOOD]
    tp = sum(r.id in hit for r in bad)
    fp = sum(r.id in hit for r in good)
    tpr = tp / len(bad) if bad else 0.0
    fpr = fp / len(good) if good else 0.0
    per: dict[str, dict[str, Any]] = {}
    for klass in sorted({r.klass for r in rows if r.label in (BAD, GOOD)}):
        b = [r for r in bad if r.klass == klass]
        g = [r for r in good if r.klass == klass]
        ktp = sum(r.id in hit for r in b)
        kfp = sum(r.id in hit for r in g)
        per[klass] = {"tpr": ktp / len(b) if b else None, "fpr": kfp / len(g) if g else None,
                      "score": (ktp / len(b) if b else 0) - (kfp / len(g) if g else 0)}
    return {"tp": tp, "fp": fp, "precision": round(tp / (tp + fp), 4) if tp + fp else None,
            "tpr": round(tpr, 4), "fpr": round(fpr, 4), "score": round(tpr - fpr, 4), "per_class": per}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("corpus")
    ap.add_argument("--a", required=True, help="first command label (e.g. rules)")
    ap.add_argument("--b", required=True, help="second command label (e.g. codeql)")
    ap.add_argument("--merged", help="label of the in-product merge (e.g. combined)")
    ap.add_argument("--bench", type=Path)
    ap.add_argument("--slack", type=int, default=3)
    ap.add_argument("--cwe-map", type=Path)
    args = ap.parse_args()
    paths = Paths((args.bench or default_bench()).resolve())
    summary = json.loads((paths.corpus_results(args.corpus) / "run_summary.json").read_text())
    units = enumerate_units(paths.bench, args.corpus, summary.get("sample"), summary.get("seed", 1),
                            summary.get("cwes"))
    # Only the rows of what was staged (a Juliet sample, not the corpus).
    whole = any(u.sources == ["."] for u in units)
    staged = {s for u in units for s in u.sources}
    rows = load_rows(paths.bench / args.corpus / "ground_truth.jsonl",
                     None if whole else lambda f: f in staged or f.split("/", 1)[0] in staged)
    index = FileIndex(rows)
    by_tool: dict[str, list[Finding]] = defaultdict(list)
    for unit in units:
        _, found = load_unit(paths.raw(args.corpus), unit.name, unit.prefix)
        for tool, fs in found.items():
            by_tool[tool].extend(fs)
    cwes = rule_cwes(paths, args.corpus, [u.name for u in units], args.cwe_map)
    a, b = by_tool.get(args.a, []), by_tool.get(args.b, [])
    if not a or not b:
        raise SystemExit(f"no findings for {args.a!r} or {args.b!r} (have {sorted(by_tool)})")

    sets: dict[str, list[Finding]] = {
        f"{args.a} alone": a,
        f"{args.b} alone": b,
        "union": a + b,
        "intersection": corroborated(a, b, cwes) + corroborated(b, a, cwes),
    }
    if args.merged and by_tool.get(args.merged):
        sets[f"{args.merged} (in-product merge)"] = by_tool[args.merged]
    result: dict[str, Any] = {"corpus": args.corpus, "findings": {}, "cases": {}}
    for name, fs in sets.items():
        result["findings"][name] = finding_view(fs, rows, args.slack)
    hits = {}
    for match in (False, True):
        view = "cwe" if match else "any"
        fa = flagged(a, index, cwes, args.slack, match)
        fb = flagged(b, index, cwes, args.slack, match)
        hits[view] = {f"{args.a} alone": fa, f"{args.b} alone": fb, "union": fa | fb, "intersection": fa & fb}
        result["cases"][view] = {name: case_view(h, rows) for name, h in hits[view].items()}
    uncovered = Counter(f.rule for f in a + b if f.rule not in cwes or not cwes[f.rule])
    result["rules_without_cwe"] = dict(uncovered.most_common(20))

    out = paths.corpus_results(args.corpus) / f"combine-{args.a}-{args.b}.json"
    out.write_text(json.dumps(result, indent=1))
    lines = [[name, v["findings"], v["tp"], v["fp"], pct(v["precision"]), pct(v["recall"])]
             for name, v in result["findings"].items()]
    md = [f"# {args.corpus}: `{args.a}` × `{args.b}`\n", "## Findings (bugbench scoring)\n",
          table(["set", "findings", "TP", "FP", "precision", "recall"], lines)]
    for view, title in (("any", "any finding in the test case"), ("cwe", "a finding of the test's CWE")):
        rows_md = [[name, v["tp"], v["fp"], pct(v["precision"]), pct(v["tpr"]), pct(v["fpr"]), pct(v["score"])]
                   for name, v in result["cases"][view].items()]
        md += [f"\n## Test cases flagged by {title}\n",
               table(["set", "TP", "FP", "precision", "TPR", "FPR", "TPR−FPR"], rows_md)]
    klasses = sorted(result["cases"]["cwe"]["union"]["per_class"])
    per = [[k] + [pct(result["cases"]["cwe"][name]["per_class"][k]["score"]) for name in hits["cwe"]]
           for k in klasses]
    md += ["\n## Benchmark score (TPR−FPR) per class, CWE-matched\n", table(["class", *hits["cwe"]], per)]
    text = "\n".join(md) + "\n"
    (paths.corpus_results(args.corpus) / f"combine-{args.a}-{args.b}.md").write_text(text)
    print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())

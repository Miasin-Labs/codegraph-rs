#!/usr/bin/env python3
"""Score stored detector output against ground truth.

    score.py <corpus>... [--slack N] [--samples N]

Reads <bench>/results/<corpus>/{run_summary.json,runs.jsonl,raw/} and scores
each command (tool) separately, so `bugs` and a rules engine never mix.
Writes metrics.json, findings.jsonl, report.md and samples-<tool>.md beside
them, and <bench>/results/summary.md over every scored corpus.
"""

from __future__ import annotations

import argparse
import json
import sys
from collections.abc import Callable
from pathlib import Path
from typing import Any

from config import Paths, default_bench
from differential import PairInput, region_rows, score_pairs
from findings import Finding, load_unit
from gt import GtRow, load_rows
from labeled import score_labeled
from report import differential_markdown, labeled_markdown, pct, samples_markdown, table
from units import Unit, enumerate_units, expand_corpus, load_advisories

Scored = list[tuple[Finding, str]]
Scorer = Callable[[dict[str, list[Finding]]], tuple[dict[str, Any], Scored]]

LABELED_GROUPS = [("TP (inside a bad row)", "tp"), ("FP (inside a good row)", "fp"),
                  ("Unlabeled, most confident", "unlabeled")]
DIFF_GROUPS = [("TP (fix region, gone in fixed/)", "tp"),
               ("Not discriminating (fix region, still in fixed/)", "not_discriminating"),
               ("Vuln-only, off the fix", "vuln_only_elsewhere"),
               ("Background, most confident", "background")]


def runtime(out: Path, units: list[Unit]) -> tuple[dict[str, Any], set[str]]:
    summary = json.loads((out / "run_summary.json").read_text())
    runs = [json.loads(line) for line in (out / "runs.jsonl").read_text().splitlines() if line.strip()]
    ok = {r["unit"] for r in runs if r["status"] == "ok"}
    detect = sum(float(c.get("seconds", 0)) for r in runs for c in r["commands"].values())
    return {
        "units": len(units), "ok": len(ok), "failed": len(runs) - len(ok),
        "failures": {r["unit"]: r["status"] for r in runs if r["status"] != "ok"},
        "wall_s": summary["wall_s"], "index_s": round(sum(r["index_s"] for r in runs), 1),
        "detect_s": round(detect, 1), "corpus_untouched": summary["corpus_untouched"],
    }, ok


def load_findings(paths: Paths, corpus: str, units: list[Unit], ok: set[str]) -> dict[str, dict[str, list[Finding]]]:
    """`{tool: {unit: findings}}` over every indexed unit."""
    out: dict[str, dict[str, list[Finding]]] = {}
    for unit in units:
        if unit.name in ok:
            _, by_tool = load_unit(paths.raw(corpus), unit.name, unit.prefix)
            for tool, findings in by_tool.items():
                out.setdefault(tool, {})[unit.name] = findings
    return out


def dump_findings(path: Path, rows: Scored) -> None:
    with open(path, "w") as fh:
        for f, outcome in rows:
            fh.write(json.dumps({"unit": f.unit, "rule": f.rule_id, "file": f.file, "line": f.line,
                                 "function": f.function, "confidence": f.confidence,
                                 "outcome": outcome, "message": f.message}) + "\n")


def spread(items: Scored, n: int) -> Scored:
    """Up to n, most confident first, round-robin over rules so one noisy rule can't fill the list."""
    by_rule: dict[str, Scored] = {}
    for it in sorted(items, key=lambda it: (-it[0].confidence, it[0].file, it[0].line)):
        by_rule.setdefault(it[0].rule_id, []).append(it)
    picked: Scored = []
    while len(picked) < n and any(by_rule.values()):
        for rule in sorted(by_rule):
            if by_rule[rule] and len(picked) < n:
                picked.append(by_rule[rule].pop(0))
    return picked


def labeled_scorer(paths: Paths, corpus: str, units: list[Unit], ok: set[str], slack: int) -> Scorer:
    whole = any(u.sources == ["."] for u in units)
    staged = {s for u in units if u.name in ok for s in u.sources}
    # a staged source is a file, or a directory whose files all count
    rows: list[GtRow] = load_rows(paths.bench / corpus / "ground_truth.jsonl",
                                  None if whole else
                                  lambda f: f in staged or f.split("/", 1)[0] in staged)

    def score(by_unit: dict[str, list[Finding]]) -> tuple[dict[str, Any], Scored]:
        metrics = score_labeled([f for fs in by_unit.values() for f in fs], rows, slack)
        verdicts = metrics.pop("verdicts")
        scored = [(v.finding, v.outcome + (" flaw-line" if v.flaw_hit else "") +
                   ("" if not v.rows else f" [{v.rows[0].klass} {v.rows[0].function or v.rows[0].category}]"))
                  for v in verdicts]
        return metrics | {"slack": slack}, scored

    return score


def rustsec_scorer(paths: Paths, corpus: str, units: list[Unit], ok: set[str], slack: int) -> Scorer:
    advisories = {str(a["advisory"]): a for a in load_advisories(paths.bench)}
    ids = sorted({str(u.meta["advisory"]) for u in units})
    wanted = tuple(f"{i}/" for i in ids)
    rows = load_rows(paths.bench / corpus / "ground_truth.jsonl", lambda f: f.startswith(wanted))
    rows_by_adv: dict[str, list[GtRow]] = {}
    for row in rows:
        rows_by_adv.setdefault(row.file.split("/", 1)[0], []).append(row)

    def score(by_unit: dict[str, list[Finding]]) -> tuple[dict[str, Any], Scored]:
        pairs: list[PairInput] = []
        for adv_id in ids:
            adv = advisories[adv_id]
            vuln, fixed = str(adv["vuln_dir"]), str(adv["fixed_dir"])
            if vuln not in ok or fixed not in ok:
                continue
            cats = [str(c) for c in adv.get("categories") or []] or ["(none)"]
            if adv.get("informational") == "unsound":
                cats.append("informational:unsound")
            pairs.append(PairInput(adv_id, cats, str(adv["localization"]), by_unit.get(vuln, []),
                                   by_unit.get(fixed, []), region_rows(adv_id, rows_by_adv.get(adv_id, []))))
        metrics = score_pairs(pairs, slack)
        verdicts = metrics.pop("verdicts")
        return metrics | {"slack": slack}, [(v.finding, v.outcome) for v in verdicts]

    return score


def score_corpus(paths: Paths, corpus: str, units: list[Unit], tools: list[str], slack: int | None,
                 n: int) -> dict[str, Any]:
    out = paths.corpus_results(corpus)
    rt, ok = runtime(out, units)
    by_tool = load_findings(paths, corpus, units, ok)
    differential = corpus == "rustsec-adjacent"
    if differential:
        scorer = rustsec_scorer(paths, corpus, units, ok, 5 if slack is None else slack)
    else:
        scorer = labeled_scorer(paths, corpus, units, ok, 3 if slack is None else slack)
    render = differential_markdown if differential else labeled_markdown
    groups = DIFF_GROUPS if differential else LABELED_GROUPS
    for stale in out.glob("samples*.md"):
        stale.unlink()
    per_tool: dict[str, dict[str, Any]] = {}
    all_scored: Scored = []
    report: list[str] = []
    for tool in tools:
        metrics, scored = scorer(by_tool.get(tool, {}))
        metrics |= {"corpus": corpus, "tool": tool, "runtime": rt}
        per_tool[tool] = metrics
        all_scored += scored
        report.append(render(f"{corpus} — `{tool}`", metrics))
        picked = {name: spread([s for s in scored if s[1].split(" ")[0] == key], n) for name, key in groups}
        (out / f"samples-{tool}.md").write_text(samples_markdown(f"{corpus} `{tool}`", picked, paths.bench / corpus))
    dump_findings(out / "findings.jsonl", all_scored)
    result: dict[str, Any] = {"corpus": corpus, "runtime": rt, "tools": per_tool}
    (out / "metrics.json").write_text(json.dumps(result, indent=1))
    (out / "report.md").write_text("\n".join(report))
    return result


def summary(paths: Paths) -> str:
    lines: list[list[object]] = []
    for metrics_path in sorted(paths.results.rglob("metrics.json")):
        result = json.loads(metrics_path.read_text())
        rt = result["runtime"]
        for tool, m in result["tools"].items():
            o = m["overall"]
            head = [result["corpus"], tool, f"{rt['ok']}/{rt['units']}", rt["wall_s"]]
            if "per_pair" in m:
                lines.append(head + [o["vuln_findings"], o["tp"], f"{o['not_discriminating']} (not disc.)",
                                     f"{o['vuln_only_elsewhere'] + o['background']} (off fix)", "-",
                                     pct(o["precision_differential"]),
                                     f"{pct(o['recall_pairs'])} of {m['scoreable_pairs']} pairs"])
            else:
                lines.append(head + [o["findings"], o["tp"], o["fp"], o["unlabeled"], pct(m["base_rate"]),
                                     pct(o["precision"]), f"{pct(o['recall'])} of {m['rows']['bad']} bad rows"])
    md = "# bugbench summary\n\n" + table(
        ["corpus", "tool", "units ok", "wall s", "findings", "TP", "FP", "unlabeled", "base rate",
         "precision", "recall"], lines) + "\n"
    (paths.results / "summary.md").write_text(md)
    return md


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("corpus", nargs="*")
    ap.add_argument("--bench", type=Path)
    ap.add_argument("--slack", type=int, help="line/hunk slack (default 3; rustsec 5)")
    ap.add_argument("--samples", type=int, default=15, help="findings per group in samples-<tool>.md")
    args = ap.parse_args()
    paths = Paths((args.bench or default_bench()).resolve())
    for name in args.corpus:
        for corpus in expand_corpus(name):
            run_summary = json.loads((paths.corpus_results(corpus) / "run_summary.json").read_text())
            units = enumerate_units(paths.bench, corpus, run_summary.get("sample"), run_summary.get("seed", 1),
                                    run_summary.get("cwes"))
            result = score_corpus(paths, corpus, units, list(run_summary["commands"]), args.slack, args.samples)
            for tool, m in result["tools"].items():
                print(f"[{corpus} {tool}] {json.dumps(m['overall'])}", file=sys.stderr)
    print(summary(paths))
    return 0


if __name__ == "__main__":
    sys.exit(main())

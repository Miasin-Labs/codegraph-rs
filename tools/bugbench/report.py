"""Markdown rendering of the scored metrics."""

from __future__ import annotations

from collections.abc import Iterable, Sequence
from pathlib import Path
from typing import Any

from findings import Finding


def pct(value: float | None) -> str:
    return "-" if value is None else f"{100 * float(value):.1f}%"


def table(header: Sequence[str], rows: Iterable[Sequence[object]]) -> str:
    lines = ["| " + " | ".join(header) + " |", "|" + "---|" * len(header)]
    lines += ["| " + " | ".join(str(c) for c in row) + " |" for row in rows]
    return "\n".join(lines)


def runtime_block(runtime: dict[str, Any]) -> str:
    return table(
        ["units", "indexed ok", "failed/timeout", "wall (s)", "index CPU-s (sum)", "detector s (sum)", "corpus untouched"],
        [[runtime["units"], runtime["ok"], runtime["failed"], runtime["wall_s"],
          runtime["index_s"], runtime["detect_s"], runtime["corpus_untouched"]]],
    )


def labeled_markdown(corpus: str, metrics: dict[str, Any]) -> str:
    rows = metrics["rows"]
    out = [f"# {corpus}\n", runtime_block(metrics["runtime"]), "",
           f"Ground-truth rows in scored files: {rows['bad']} bad, {rows['good']} good.  "
           f"Line-row slack: +-{metrics['slack']} lines. Base rate (precision of a detector firing "
           f"at random inside labeled rows): {pct(metrics['base_rate'])}.\n", "## Per rule\n"]
    rule_rows = [("ALL", metrics["overall"])] + sorted(metrics["per_rule"].items())
    out.append(table(
        ["rule", "findings", "TP", "FP", "unlabeled", "flaw-line hits", "precision", "bad hit", "recall", "good flagged"],
        [[r, s["findings"], s["tp"], s["fp"], s["unlabeled"], s["flaw_line_hits"], pct(s["precision"]),
          s["bad_hit"], pct(s["recall"]), s["good_flagged"]] for r, s in rule_rows],
    ))
    out.append("\n## Per CWE / category\n")
    out.append(table(
        ["class", "bad", "good", "bad hit", "recall", "good flagged", "precision", "TP by rule", "FP by rule"],
        [[k, c["bad"], c["good"], c["bad_hit"], pct(c["recall"]), c["good_flagged"], pct(c["precision"]),
          _counts(c["tp_by_rule"]), _counts(c["fp_by_rule"])]
         for k, c in metrics["per_class"].items()],
    ))
    return "\n".join(out) + "\n"


def differential_markdown(corpus: str, metrics: dict[str, Any]) -> str:
    out = [f"# {corpus} (differential)\n", runtime_block(metrics["runtime"]), "",
           f"{metrics['pairs']} pairs, {metrics['scoreable_pairs']} with a located fix region; "
           f"hunk slack +-{metrics['slack']} lines.\n",
           "`tp` = in the fix region and gone in fixed/; `not_disc` = in the region but still in fixed/; "
           "`vuln-only elsewhere` = gone in fixed/ but off the fix; `background` = off the fix and in both.\n",
           "## Per rule\n"]
    rule_rows = [("ALL", metrics["overall"])] + sorted(metrics["per_rule"].items())
    out.append(table(
        ["rule", "vuln findings", "TP", "not disc.", "vuln-only elsewhere", "background", "introduced",
         "pairs detected", "recall (pairs)", "precision (of vuln-only)", "discriminating in region"],
        [[r, s["vuln_findings"], s["tp"], s["not_discriminating"], s["vuln_only_elsewhere"], s["background"],
          s["introduced"], s["pairs_detected"], pct(s["recall_pairs"]), pct(s["precision_differential"]),
          pct(s["discriminating_share_in_region"])] for r, s in rule_rows],
    ))
    out.append("\n## Per advisory category\n")
    out.append(table(
        ["category", "pairs", "scoreable", "detected", "recall", "findings in region"],
        [[k, c.get("pairs", 0), c.get("scoreable", 0), c.get("detected", 0), pct(c.get("recall")),
          c.get("region_findings", 0)] for k, c in metrics["per_category"].items()],
    ))
    return "\n".join(out) + "\n"


def _counts(counts: dict[str, int]) -> str:
    return ", ".join(f"{k.split(':', 1)[-1]} {v}" for k, v in sorted(counts.items(), key=lambda kv: -kv[1])) or "-"


def snippet(path: Path, line: int, radius: int = 3) -> str:
    try:
        lines = path.read_text(errors="replace").splitlines()
    except OSError:
        return "    (source unavailable)"
    lo, hi = max(1, line - radius), min(len(lines), line + radius)
    return "\n".join(f"{'>' if n == line else ' '}{n:5} {lines[n - 1][:160]}" for n in range(lo, hi + 1))


def samples_markdown(title: str, groups: dict[str, list[tuple[Finding, str]]], source_root: Path) -> str:
    """Findings with a few lines of source each, for reading by hand."""
    out = [f"# {title}: sampled findings\n"]
    for name, items in groups.items():
        out.append(f"## {name} ({len(items)} shown)\n")
        for finding, note in items:
            out.append(f"### {finding.rule} `{finding.file}:{finding.line}` "
                       f"({finding.confidence:.2f}{', in ' + finding.function if finding.function else ''})\n")
            out.append(f"{finding.message}  \n{note}\n")
            out.append("```\n" + snippet(source_root / finding.file, finding.line) + "\n```\n")
    return "\n".join(out)

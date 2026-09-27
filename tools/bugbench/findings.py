"""Load the stored detector output as normalized findings."""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from runner import extract_findings, safe_name


@dataclass(frozen=True)
class Finding:
    unit: str
    tool: str  # command label: `bugs`, `rules`, …
    rule: str
    file: str  # ground-truth path (unit prefix + project-relative path)
    local_file: str  # project-relative path as reported
    line: int
    function: str | None
    message: str
    confidence: float
    detector: str

    @property
    def rule_id(self) -> str:
        """`tool:rule`, so two engines' rules never merge."""
        return f"{self.tool}:{self.rule}"


def load_unit(raw_dir: Path, unit: str, prefix: str) -> tuple[dict[str, Any], dict[str, list[Finding]]]:
    """`(run.json, {tool label: findings})` for one unit."""
    base = raw_dir / safe_name(unit)
    run_path = base / "run.json"
    run: dict[str, Any] = json.loads(run_path.read_text()) if run_path.exists() else {"status": "missing"}
    by_tool: dict[str, list[Finding]] = {}
    for path in sorted(base.glob("*.json")):
        if path.name == "run.json":
            continue
        tool = path.stem
        try:
            payload = json.loads(path.read_text())
        except ValueError:
            continue
        by_tool[tool] = [_normalize(unit, tool, prefix, f) for f in extract_findings(payload)]
    return run, by_tool


def _normalize(unit: str, tool: str, prefix: str, raw: dict[str, Any]) -> Finding:
    local = str(raw.get("file") or raw.get("path") or "")
    rule = raw.get("rule") or raw.get("ruleId") or raw.get("id") or "?"
    return Finding(
        unit=unit,
        tool=tool,
        rule=str(rule),
        file=prefix + local,
        local_file=local,
        line=int(raw.get("line") or raw.get("startLine") or 0),
        function=raw.get("function") if isinstance(raw.get("function"), str) else None,
        message=str(raw.get("message") or ""),
        confidence=float(raw.get("confidence") or 0.0),
        detector=str(raw.get("detector") or tool),
    )

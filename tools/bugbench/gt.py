"""Ground truth: rows of `<corpus>/ground_truth.jsonl`, indexed by file."""

from __future__ import annotations

import json
from collections.abc import Callable, Iterable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

BAD = "bad"
GOOD = "good"


@dataclass(frozen=True)
class GtRow:
    id: int
    file: str
    label: str
    granularity: str
    line_start: int | None
    line_end: int | None
    function: str | None
    cwe: str | None
    category: str | None
    flaw_lines: tuple[int, ...] = ()
    extra: dict[str, Any] = field(default_factory=dict, compare=False, hash=False)

    @property
    def klass(self) -> str:
        """The class a row is scored under: CWE, else category, else `?`."""
        return self.cwe or self.category or "?"

    def covers(self, line: int, slack: int) -> bool:
        """Does a finding at `line` fall on this row?

        Function rows: inside the function. Line rows: within `slack` lines
        of the span. File rows (or rows with no lines): anywhere in the file.
        """
        if self.granularity == "file" or self.line_start is None:
            return True
        end = self.line_end if self.line_end is not None else self.line_start
        pad = slack if self.granularity == "line" else 0
        return self.line_start - pad <= line <= end + pad


_CORE = {"file", "label", "granularity", "line_start", "line_end", "function", "cwe",
         "category", "flaw_lines"}


def load_rows(path: Path, keep: Callable[[str], bool] | None = None) -> list[GtRow]:
    """Rows whose `file` passes `keep` (all when None)."""
    rows: list[GtRow] = []
    with open(path) as fh:
        for line in fh:
            if not line.strip():
                continue
            raw = json.loads(line)
            file = str(raw["file"])
            if keep is not None and not keep(file):
                continue
            rows.append(GtRow(
                id=len(rows),
                file=file,
                label=str(raw.get("label", "")),
                granularity=str(raw.get("granularity") or ("line" if raw.get("line_start") else "file")),
                line_start=raw.get("line_start"),
                line_end=raw.get("line_end"),
                function=raw.get("function"),
                cwe=raw.get("cwe"),
                category=raw.get("category"),
                flaw_lines=tuple(raw.get("flaw_lines") or ()),
                extra={k: v for k, v in raw.items() if k not in _CORE},
            ))
    return rows


class FileIndex:
    """Rows grouped by file for point lookups."""

    def __init__(self, rows: Iterable[GtRow]) -> None:
        self.by_file: dict[str, list[GtRow]] = {}
        for row in rows:
            self.by_file.setdefault(row.file, []).append(row)

    def at(self, file: str, line: int, slack: int, labels: set[str] | None = None) -> list[GtRow]:
        return [
            row for row in self.by_file.get(file, ())
            if (labels is None or row.label in labels) and row.covers(line, slack)
        ]

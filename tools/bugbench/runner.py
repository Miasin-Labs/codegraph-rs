"""Stage one unit, index it with codegraph, run the detector commands."""

from __future__ import annotations

import json
import shlex
import shutil
import subprocess
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any

from config import Paths
from units import Unit

IGNORE_ON_COPY = shutil.ignore_patterns(".codegraph", "ground_truth.jsonl", "results")


@dataclass
class UnitRun:
    corpus: str
    unit: str
    status: str = "ok"  # ok | index-failed | index-timeout | stage-failed
    files_staged: int = 0
    index_s: float = 0.0
    commands: dict[str, dict[str, Any]] = field(default_factory=dict)
    error: str = ""

    def to_json(self) -> dict[str, Any]:
        return asdict(self)


def safe_name(unit: str) -> str:
    return unit.replace("/", "__")


def stage(bench: Path, unit: Unit, dest: Path) -> int:
    """Copy the unit's sources into `dest` (fresh inodes: never a hardlink)."""
    if dest.exists():
        shutil.rmtree(dest)
    src_root = bench / unit.gt_dir
    if len(unit.sources) == 1 and (src_root / unit.sources[0]).is_dir():
        shutil.copytree(src_root / unit.sources[0], dest, ignore=IGNORE_ON_COPY,
                        copy_function=shutil.copyfile, symlinks=True)
    else:
        for rel in unit.sources:
            target = dest / rel
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(src_root / rel, target)
    return sum(1 for p in dest.rglob("*") if p.is_file())


def run_cmd(argv: list[str], env: dict[str, str], timeout: float) -> tuple[int | None, str, str, float]:
    start = time.monotonic()
    try:
        proc = subprocess.run(argv, env=env, capture_output=True, text=True, timeout=timeout)
        return proc.returncode, proc.stdout, proc.stderr, time.monotonic() - start
    except subprocess.TimeoutExpired as exc:
        err = exc.stderr.decode(errors="replace") if isinstance(exc.stderr, bytes) else ""
        return None, "", err, time.monotonic() - start


def run_unit(
    paths: Paths,
    binary: Path,
    unit: Unit,
    commands: dict[str, str],
    env: dict[str, str],
    timeout: float,
    keep: bool,
) -> UnitRun:
    result = UnitRun(unit.corpus, unit.name)
    root = paths.work / unit.corpus / safe_name(unit.name)
    out_dir = paths.raw(unit.corpus) / safe_name(unit.name)
    if out_dir.exists():
        shutil.rmtree(out_dir)  # no stale output of a command this run doesn't make
    out_dir.mkdir(parents=True)
    try:
        result.files_staged = stage(paths.bench, unit, root)
    except OSError as exc:
        result.status, result.error = "stage-failed", str(exc)
        return result

    code, _, err, elapsed = run_cmd([str(binary), "init", str(root)], env, timeout)
    result.index_s = round(elapsed, 3)
    if code != 0:
        result.status = "index-timeout" if code is None else "index-failed"
        result.error = err[-2000:]
    else:
        for label, command in commands.items():
            argv = [str(binary), *shlex.split(command), "-p", str(root)]
            code, out, err, elapsed = run_cmd(argv, env, timeout)
            entry: dict[str, Any] = {"command": command, "seconds": round(elapsed, 3),
                                        "exit": code}
            if code == 0:
                (out_dir / f"{label}.json").write_text(out)
                entry["findings"] = count_findings(out)
            else:
                entry["error"] = ("timeout" if code is None else err[-2000:])
            result.commands[label] = entry
    (out_dir / "run.json").write_text(json.dumps(result.to_json(), indent=1))
    if not keep:
        shutil.rmtree(root, ignore_errors=True)
    return result


def count_findings(stdout: str) -> int | None:
    try:
        return len(extract_findings(json.loads(stdout)))
    except (ValueError, TypeError):
        return None


def extract_findings(payload: object) -> list[dict[str, Any]]:
    """Findings from a `{kind, data: {findings}}` envelope or a bare report."""
    node = payload
    if isinstance(node, dict) and isinstance(node.get("data"), dict):
        node = node["data"]
    if isinstance(node, dict):
        node = node.get("findings", [])
    return [f for f in node if isinstance(f, dict)] if isinstance(node, list) else []

"""Paths, environment and command lines shared by the bugbench scripts."""

from __future__ import annotations

import os
import shlex
from dataclasses import dataclass
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]

# The detector command every run makes. `-p <root>` is appended per unit.
DEFAULT_COMMANDS: dict[str, str] = {
    "bugs": "analyze bugs --json --tests --top 100000",
}

# Environment for every codegraph child: a scratch home, and nothing that
# registers, builds dependency shards, spawns background work, or reaches
# into other graphs. Never the developer's ~/.codegraph.
ISOLATION_ENV: dict[str, str] = {
    "CODEGRAPH_ATLAS": "0",
    "CODEGRAPH_DEPS": "0",
    "CODEGRAPH_NO_BACKGROUND_SYNC": "1",
    "CODEGRAPH_EXTERNAL": "0",
    "CODEGRAPH_HISTORY": "0",
    "CODEGRAPH_RUST_DEPS": "0",
    "NO_COLOR": "1",
}


@dataclass(frozen=True)
class Paths:
    """Where the corpora live and where the harness writes."""

    bench: Path  # the directory holding MANIFEST.md and the corpora

    @property
    def home(self) -> Path:
        """Scratch CODEGRAPH_HOME, a sibling of the bench dir."""
        return self.bench.parent / "cghome-bench"

    @property
    def work(self) -> Path:
        """Staged copies of the units (the corpora are never indexed in place)."""
        return self.bench.parent / "bugbench-work"

    @property
    def results(self) -> Path:
        return self.bench / "results"

    def corpus_results(self, corpus: str) -> Path:
        return self.results / corpus

    def raw(self, corpus: str) -> Path:
        return self.corpus_results(corpus) / "raw"


def default_bench() -> Path:
    env = os.environ.get("BUGBENCH_ROOT")
    if env:
        return Path(env)
    raise SystemExit("set BUGBENCH_ROOT or pass --bench <dir holding MANIFEST.md>")


def default_binary() -> Path:
    for profile in ("release", "debug"):
        candidate = REPO_ROOT / "target" / profile / "codegraph"
        if candidate.exists():
            return candidate
    raise SystemExit("no codegraph binary; run `cargo build --release --bin codegraph`")


def child_env(paths: Paths) -> dict[str, str]:
    env = dict(os.environ)
    env.update(ISOLATION_ENV)
    env["CODEGRAPH_HOME"] = str(paths.home)
    return env


def parse_commands(specs: list[str] | None, include_default: bool = True) -> dict[str, str]:
    """The default `bugs` command plus each `--cmd 'label=analyze rules --builtin'`.

    Without `label=` the label is the command's second word (`rules`).
    """
    commands: dict[str, str] = dict(DEFAULT_COMMANDS) if include_default else {}
    for spec in specs or []:
        label, sep, rest = spec.partition("=")
        if sep and label.strip() and " " not in label.strip():
            commands[label.strip()] = rest.strip()
        else:
            words = shlex.split(spec)
            commands[words[1] if len(words) > 1 else words[0]] = spec.strip()
    return commands

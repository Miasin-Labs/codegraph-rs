"""What gets indexed: a corpus split into units, each a directory to stage.

A unit is staged as a *copy* under the work dir, laid out so that a
finding's project-relative path plus the unit's `prefix` is the path the
corpus ground truth uses. The corpora are never indexed in place (the
rustsec trees hardlink identical files across versions).
"""

from __future__ import annotations

import json
import random
import re
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

# Juliet testcase id: `<CWE..>_NN` before the optional `a`..`z` part and role.
JULIET_ID = re.compile(r"^(.*_\d\d)[a-z]?(?:_[A-Za-z]\w*)?\.(c|cpp|h|java)$")

# CWEs sampled by default: the detectors' own bug shapes (dead store,
# constant condition, unchecked result, infinite loop, operator mistakes)
# plus the common memory/injection classes a rules engine will target.
JULIET_C_CWES = [
    "CWE121", "CWE122", "CWE134", "CWE190", "CWE252", "CWE253", "CWE369", "CWE398",
    "CWE401", "CWE415", "CWE416", "CWE476", "CWE480", "CWE481", "CWE482", "CWE483",
    "CWE484", "CWE563", "CWE570", "CWE571", "CWE78", "CWE835",
]
JULIET_JAVA_CWES = [
    "CWE129", "CWE190", "CWE252", "CWE253", "CWE369", "CWE396", "CWE398", "CWE476",
    "CWE478", "CWE481", "CWE482", "CWE483", "CWE484", "CWE561", "CWE563", "CWE570",
    "CWE571", "CWE690", "CWE78", "CWE80", "CWE835", "CWE89",
]
# Juliet support files that every testcase includes (the generated
# main/testcases.h are tens of MB and carry no flaws).
JULIET_SUPPORT = {
    "juliet-c": ("testcasesupport", {"io.c", "std_testcase.h", "std_testcase_io.h",
                                     "std_thread.c", "std_thread.h"}),
    "juliet-java": ("src/testcasesupport", None),
}
JULIET_TESTCASES = {"juliet-c": "testcases", "juliet-java": "src/testcases"}

WEBAPPS = ["juice-shop", "DVWA", "NodeGoat", "pygoat", "Vulnerable-Flask-App"]

# rustsec-adjacent pairs taken by default: small diffs with a located fix.
RUSTSEC_TIGHTNESS = {"tight"}
RUSTSEC_LOCALIZED = {"fix_commit", "rudra", "name_match", "tight_diff"}


@dataclass
class Unit:
    corpus: str  # results key, e.g. `juliet-c`, `webapps/DVWA`
    name: str  # unique within the corpus; also the raw-output dir
    gt_dir: str  # corpus dir (relative to bench) whose ground_truth.jsonl applies
    prefix: str  # prepended to a finding's path to get the ground-truth path
    # corpus-relative source paths to copy (files or whole directories)
    sources: list[str] = field(default_factory=list)
    meta: dict[str, Any] = field(default_factory=dict)


def expand_corpus(corpus: str) -> list[str]:
    if corpus == "webapps":
        return [f"webapps/{app}" for app in WEBAPPS]
    return [corpus]


def enumerate_units(bench: Path, corpus: str, sample: int | None, seed: int) -> list[Unit]:
    if corpus in JULIET_TESTCASES:
        return juliet_units(bench, corpus, sample or 40, seed)
    if corpus == "rustsec-adjacent":
        return rustsec_units(bench, corpus, sample, seed)
    if corpus == "owasp-benchmark-java" or corpus.startswith("webapps/"):
        return [Unit(corpus, "all", corpus, "", ["."])]
    raise SystemExit(f"unknown corpus {corpus!r}")


def juliet_units(bench: Path, corpus: str, per_cwe: int, seed: int) -> list[Unit]:
    """One unit per sampled CWE: up to `per_cwe` whole testcases + support."""
    root = bench / corpus
    testcases = root / JULIET_TESTCASES[corpus]
    wanted = JULIET_C_CWES if corpus == "juliet-c" else JULIET_JAVA_CWES
    support_dir, support_names = JULIET_SUPPORT[corpus]
    support = sorted(
        str(p.relative_to(root))
        for p in (root / support_dir).iterdir()
        if p.is_file() and (support_names is None or p.name in support_names)
    )
    rng = random.Random(seed)
    units: list[Unit] = []
    for cwe_dir in sorted(testcases.iterdir()):
        if not cwe_dir.is_dir() or cwe_dir.name.split("_")[0] not in wanted:
            continue
        by_id: dict[str, list[str]] = {}
        for path in sorted(cwe_dir.rglob("*")):
            match = JULIET_ID.match(path.name) if path.is_file() else None
            if match:
                by_id.setdefault(match.group(1), []).append(str(path.relative_to(root)))
        ids = sorted(by_id)
        chosen = sorted(rng.sample(ids, min(per_cwe, len(ids))))
        files = [f for tc in chosen for f in by_id[tc]]
        units.append(Unit(corpus, cwe_dir.name, corpus, "", files + support,
                          {"cwe": cwe_dir.name.split("_")[0], "testcases": chosen,
                           "testcases_total": len(ids)}))
    return units


def load_advisories(bench: Path) -> list[dict[str, Any]]:
    with open(bench / "rustsec-adjacent" / "advisories.jsonl") as fh:
        return [json.loads(line) for line in fh if line.strip()]


def rustsec_units(bench: Path, corpus: str, sample: int | None, seed: int) -> list[Unit]:
    """Each selected pair gives two units, `<id>/vuln` and `<id>/fixed`."""
    pairs = [
        a for a in load_advisories(bench)
        if a["diff_tightness"] in RUSTSEC_TIGHTNESS and a["localization"] in RUSTSEC_LOCALIZED
    ]
    pairs.sort(key=lambda a: str(a["advisory"]))
    if sample and sample < len(pairs):
        pairs = sorted(random.Random(seed).sample(pairs, sample), key=lambda a: str(a["advisory"]))
    units: list[Unit] = []
    for adv in pairs:
        for side in ("vuln", "fixed"):
            rel = str(adv[f"{side}_dir"])
            units.append(Unit(corpus, rel, corpus, rel + "/", [rel],
                              {"advisory": adv["advisory"], "side": side}))
    return units

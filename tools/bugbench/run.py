#!/usr/bin/env python3
"""Index each unit of a corpus and store the detectors' raw JSON.

    run.py <corpus> [--sample N] [--seed S] [--cwe CWE690]... [--jobs J] [--cmd 'rules=analyze rules --builtin']

Corpora: rustsec-adjacent, rudra, juliet-c, juliet-java, owasp-benchmark-java,
webapps (= webapps/<app> for each app), or one webapps/<app>.
Writes <bench>/results/<corpus>/raw/<unit>/{run.json,<label>.json} and
<bench>/results/<corpus>/runs.jsonl.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

from config import Paths, child_env, default_bench, default_binary, parse_commands
from runner import UnitRun, run_unit
from units import Unit, enumerate_units, expand_corpus


def corpus_fingerprint(bench: Path, units: list[Unit]) -> str:
    """(path, size, mtime, inode) of every source file the units read."""
    digest = hashlib.sha256()
    for unit in units:
        base = bench / unit.gt_dir
        for rel in unit.sources:
            path = base / rel
            files = sorted(path.rglob("*")) if path.is_dir() else [path]
            for file in files:
                if ".codegraph" in file.parts or not file.is_file():
                    continue
                st = file.stat()
                digest.update(f"{file}\0{st.st_size}\0{st.st_mtime_ns}\0{st.st_ino}\n".encode())
    return digest.hexdigest()


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("corpus")
    ap.add_argument("--bench", type=Path, help="dir holding MANIFEST.md (else $BUGBENCH_ROOT)")
    ap.add_argument("--binary", type=Path, help="codegraph binary (else target/{release,debug})")
    ap.add_argument("--sample", type=int, help="juliet: testcases per CWE (40); rustsec: pairs")
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--cwe", action="append", help="juliet: sample this CWE (e.g. CWE690; repeatable) "
                    "instead of the default list")
    ap.add_argument("--jobs", type=int, default=6)
    ap.add_argument("--timeout", type=float, default=900.0, help="seconds per codegraph call")
    ap.add_argument("--cmd", action="append", help="extra command 'label=analyze …' (repeatable)")
    ap.add_argument("--no-default-cmd", action="store_true", help="skip `analyze bugs`")
    ap.add_argument("--keep", action="store_true", help="keep the staged, indexed copies")
    args = ap.parse_args()

    paths = Paths((args.bench or default_bench()).resolve())
    binary = (args.binary or default_binary()).resolve()
    commands = parse_commands(args.cmd, include_default=not args.no_default_cmd)
    env = child_env(paths)
    paths.home.mkdir(parents=True, exist_ok=True)

    status = 0
    for corpus in expand_corpus(args.corpus):
        units = enumerate_units(paths.bench, corpus, args.sample, args.seed, args.cwe)
        before = corpus_fingerprint(paths.bench, units)
        out = paths.corpus_results(corpus)
        out.mkdir(parents=True, exist_ok=True)
        start = time.monotonic()
        runs: list[UnitRun] = []
        print(f"[{corpus}] {len(units)} units, jobs={args.jobs}, commands={list(commands)}",
              file=sys.stderr)
        with ThreadPoolExecutor(max_workers=max(1, args.jobs)) as pool:
            futures = {pool.submit(run_unit, paths, binary, u, commands, env, args.timeout, args.keep): u
                       for u in units}
            for n, fut in enumerate(as_completed(futures), 1):
                run = fut.result()
                runs.append(run)
                found = {k: v.get("findings") for k, v in run.commands.items()}
                print(f"  [{n}/{len(units)}] {run.unit}: {run.status} index {run.index_s:.1f}s {found}",
                      file=sys.stderr)
        wall = time.monotonic() - start
        after = corpus_fingerprint(paths.bench, units)
        runs.sort(key=lambda r: r.unit)
        with open(out / "runs.jsonl", "w") as fh:
            for run in runs:
                fh.write(json.dumps(run.to_json()) + "\n")
        summary = {
            "corpus": corpus, "units": len(units), "wall_s": round(wall, 1),
            "binary": str(binary), "commands": commands, "seed": args.seed, "sample": args.sample,
            "cwes": args.cwe,
            "status": {s: sum(r.status == s for r in runs) for s in sorted({r.status for r in runs})},
            "corpus_untouched": before == after,
            "unit_meta": {u.name: u.meta for u in units},
        }
        (out / "run_summary.json").write_text(json.dumps(summary, indent=1))
        print(f"[{corpus}] done in {wall:.0f}s: {summary['status']}; corpus untouched: "
              f"{summary['corpus_untouched']}", file=sys.stderr)
        if before != after:
            status = 1
    return status


if __name__ == "__main__":
    sys.exit(main())

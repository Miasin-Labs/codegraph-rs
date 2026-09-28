"""Ground truth for OWASP Benchmark v1.2 (Java) from expectedresults-1.2.csv.

usage: owasp_gt.py <corpus_root>

<corpus_root> is a BenchmarkJava checkout (MANIFEST.md: commit 20cbf3d1,
`.git`, `results/`, `scorecard/`, `VMs/` removed). One file-level row per
test case: `real vulnerability` true -> bad, false -> good, with the CWE,
the category, and the `doPost` method's lines (lexical, `cfuncs.py`).
"""
import collections
import csv
import json
import os
import sys

sys.path.insert(0, os.path.dirname(__file__))
from cfuncs import functions  # noqa: E402

TESTCODE = 'src/main/java/org/owasp/benchmark/testcode'


def main():
    root = sys.argv[1]
    stats = collections.Counter()
    with open(os.path.join(root, 'expectedresults-1.2.csv')) as fh, \
            open(os.path.join(root, 'ground_truth.jsonl'), 'w') as out:
        for row in csv.reader(fh):
            if not row or row[0].startswith('#'):
                continue
            name, category, real, cwe = (c.strip() for c in row[:4])
            rel = f'{TESTCODE}/{name}.java'
            try:
                text = open(os.path.join(root, rel), encoding='utf-8').read()
            except OSError:
                stats['missing_file'] += 1
                continue
            post = next((f for f in functions(text) if f['name'] == 'doPost'), None)
            label = 'bad' if real == 'true' else 'good'
            stats[label] += 1
            rec = {
                'corpus': 'owasp-benchmark-java', 'file': rel,
                'function': f'{name}.doPost' if post else None,
                'method_line_start': post['line_start'] if post else None,
                'method_line_end': post['line_end'] if post else None,
                'cwe': f'CWE-{cwe}', 'category': category, 'label': label,
                'granularity': 'file', 'source_of_truth': 'expectedresults-1.2.csv',
            }
            out.write(json.dumps({k: v for k, v in rec.items() if v is not None}) + '\n')
    print(json.dumps(stats))


main()

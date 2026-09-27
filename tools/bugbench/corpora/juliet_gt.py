"""Build ground_truth.jsonl for a Juliet v1.3 tree (C/C++ or Java).

usage: juliet_gt.py <corpus_root> <corpus_name> <src_subdir>

Function records: every function whose Juliet role name contains bad/good
(bad, badSink, badSource, good, goodG2B, goodB2GSink, helperBad, ...).
label = bad|good. flaw_lines = manifest <flaw> lines inside the function.
Manifest flaws that fall outside every recorded function are emitted as
line-level records (label bad, granularity line).
"""
import collections
import json
import os
import re
import sys
import xml.etree.ElementTree as ET
from multiprocessing import Pool

sys.path.insert(0, os.path.dirname(__file__))
from cfuncs import functions  # noqa: E402

root, corpus, sub = sys.argv[1], sys.argv[2], sys.argv[3]
EXTS = ('.c', '.cpp', '.java', '.h')
SKIP = {'main', 'runTest', 'mainFromParent'}


def cwe_of(rel):
    m = re.search(r'CWE(\d+)_', rel)
    return f'CWE-{m.group(1)}' if m else None


def role(name):
    m = re.search(r'_\d{2}[a-z]?_(\w+)$', name)
    suf = m.group(1) if m else name
    low = suf.lower()
    if 'bad' in low and 'good' not in low:
        return 'bad', suf
    if 'good' in low:
        return 'good', suf
    return None, suf


def work(rel):
    try:
        text = open(os.path.join(root, rel), encoding='latin-1').read()
    except OSError:
        return rel, []
    return rel, functions(text)


def main():
    files = []
    base = os.path.join(root, sub)
    for d, _, fs in os.walk(base):
        for f in fs:
            if f.endswith(EXTS):
                files.append(os.path.relpath(os.path.join(d, f), root))
    byname = {os.path.basename(f): f for f in files}
    flaws = collections.defaultdict(list)
    # regex, not XML: the Java manifest is not well-formed
    cur = None
    for ln in open(os.path.join(root, 'manifest.xml'), encoding='latin-1'):
        m = re.search(r'<file path="([^"]+)"', ln)
        if m:
            cur = m.group(1)
        m = re.search(r'<flaw line="(\d+)" name="([^"]+)"', ln)
        if m and cur:
            flaws[cur].append((int(m.group(1)), m.group(2)))
    out = open(os.path.join(root, 'ground_truth.jsonl'), 'w')
    stats = collections.Counter()
    with Pool(os.cpu_count()) as pool:
        for rel, funcs in pool.imap_unordered(work, sorted(files), chunksize=64):
            if '/testcases/' not in '/' + rel:
                continue
            fl = flaws.get(os.path.basename(rel), [])
            used = set()
            cwe = cwe_of(rel)
            for f in funcs:
                if f['name'] in SKIP:
                    continue
                label, suf = role(f['name'])
                if label is None:
                    # class-based variants: X_81_bad::action, X_81_goodG2B.java
                    stem = os.path.splitext(os.path.basename(rel))[0]
                    for ctx in (f['qualified'].rsplit('::', 1)[0] if '::' in f['qualified'] else '', stem):
                        l2, s2 = role(ctx) if re.search(r'_\d{2}[a-z]?_\w+$', ctx) else (None, None)
                        if l2:
                            label, suf = l2, s2 + '::' + f['name']
                            break
                if label is None:
                    continue
                inside = [(ln, n) for ln, n in fl if f['line_start'] <= ln <= f['line_end']]
                used.update(inside)
                rec = {
                    'corpus': corpus, 'file': rel, 'function': f['qualified'],
                    'line_start': f['line_start'], 'line_end': f['line_end'],
                    'cwe': cwe, 'category': rel.split('/')[-2] if re.match(r's\d\d$', rel.split('/')[-2]) is None else rel.split('/')[-3],
                    'label': label, 'role': suf, 'granularity': 'function',
                    'flaw_lines': sorted(ln for ln, _ in inside),
                    'source_of_truth': 'juliet-naming+manifest.xml',
                }
                stats[label] += 1
                if inside:
                    stats[f'{label}_with_flaw_line'] += 1
                out.write(json.dumps(rec) + '\n')
            for ln, n in fl:
                if (ln, n) in used:
                    continue
                stats['flaw_outside_function'] += 1
                out.write(json.dumps({
                    'corpus': corpus, 'file': rel, 'line_start': ln, 'line_end': ln,
                    'cwe': 'CWE-' + n.split(':')[0].split('-')[1], 'label': 'bad',
                    'granularity': 'line', 'source_of_truth': 'manifest.xml',
                }) + '\n')
    stats['manifest_flaws'] = sum(len(v) for v in flaws.values())
    stats['manifest_files_missing'] = sum(1 for k in flaws if k not in byname)
    print(json.dumps(stats))


main()

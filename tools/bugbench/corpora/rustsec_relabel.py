"""Relabel RustSec GT: only fix-localized hunks are bad/good; the rest of a
version diff is context. Writes ground_truth.jsonl (relabelled, loose-pair
plain diff hunks dropped) and advisories.jsonl (one row per advisory)."""
import collections
import json
import os
import sys

d, corpus = sys.argv[1], sys.argv[2]
STRONG = {'fix_commit', 'name_match', 'rudra_report'}
rows = [json.loads(l) for l in open(os.path.join(d, 'ground_truth.jsonl'))]
by_adv = collections.defaultdict(list)
for r in rows:
    by_adv[r.get('advisory')].append(r)
out = open(os.path.join(d, 'ground_truth.jsonl'), 'w')
adv_out = open(os.path.join(d, 'advisories.jsonl'), 'w')
st = collections.Counter()
for aid, rs in sorted(by_adv.items(), key=lambda t: t[0] or ''):
    meta = json.load(open(os.path.join(d, aid, 'meta.json')))
    tight = meta['diff_tightness']
    localized = any(r.get('relevance') in STRONG for r in rs)
    for r in rs:
        rel = r.get('relevance')
        side_bad = '/vuln/' in r['file']
        if rel in STRONG:
            r['label'] = 'bad' if side_bad else 'good'
        elif rel == 'test':
            r['label'] = 'test_change'
        elif tight == 'tight' and not localized:
            r['label'] = 'bad_candidate' if side_bad else 'good_candidate'
        else:
            if tight == 'loose':
                st['dropped_loose_context'] += 1
                continue
            r['label'] = 'context_vuln' if side_bad else 'context_fixed'
        r['diff_tightness'] = tight
        st[r['label']] += 1
        out.write(json.dumps(r) + '\n')
    bad_fns = sorted({r['function'] for r in rs if r.get('function') and r.get('relevance') in STRONG and '/vuln/' in r['file']})
    adv_out.write(json.dumps({
        'corpus': corpus, 'advisory': aid, 'crate': meta['crate'], 'vuln_version': meta['vuln_version'],
        'fixed_version': meta['fixed_version'], 'vuln_dir': f'{aid}/vuln', 'fixed_dir': f'{aid}/fixed',
        'title': meta['title'], 'categories': meta['categories'], 'keywords': meta['keywords'],
        'informational': meta['informational'], 'aliases': meta['aliases'], 'url': meta['url'],
        'diff_tightness': tight, 'diff_files': meta['diff_files'],
        'diff_lines': meta['diff_lines_added'] + meta['diff_lines_removed'],
        'localization': 'fix_commit' if any(r.get('relevance') == 'fix_commit' for r in rs) else
                        'rudra' if any(r.get('relevance') == 'rudra_report' for r in rs) else
                        'name_match' if localized else 'tight_diff' if tight == 'tight' else 'none',
        'vuln_functions': bad_fns[:50],
        'adjacent_versions': meta.get('adjacent_versions'),
        'sources': [meta['vuln_source'], meta['fixed_source']],
    }) + '\n')
    st['advisories'] += 1
print(dict(st))

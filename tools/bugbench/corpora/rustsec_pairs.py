"""Find RustSec advisories whose vulnerable and patched versions are both in
the local cargo registry cache (read-only), and list singles.

usage: rustsec_pairs.py <advisory-db> <registry-src-root> <out.json>
"""
import glob
import json
import os
import re
import sys
import tomllib

db, reg, outp = sys.argv[1:4]


def parse_ver(s):
    m = re.match(r'^(\d+)(?:\.(\d+))?(?:\.(\d+))?(?:-([0-9A-Za-z.-]+))?(?:\+.*)?$', s.strip())
    if not m:
        return None
    pre = m.group(4)
    return (int(m.group(1)), int(m.group(2) or 0), int(m.group(3) or 0), (0, pre) if pre else (1, ''))


def cmp_key(v):
    return v


def caret_upper(parts):
    a, b, c = parts[0], parts[1], parts[2]
    if a > 0:
        return (a + 1, 0, 0, (0, ''))
    if b > 0:
        return (0, b + 1, 0, (0, ''))
    return (0, 0, c + 1, (0, ''))


def matches_one(v, req):
    req = req.strip()
    m = re.match(r'^(>=|<=|>|<|=|\^|~)?\s*(.+)$', req)
    op, rest = m.group(1) or '^', m.group(2).strip()
    ncomp = rest.split('+')[0].split('-')[0].count('.') + 1
    r = parse_ver(rest)
    if r is None:
        return False
    if op == '>=':
        return v >= r
    if op == '>':
        return v > r
    if op == '<=':
        return v <= r
    if op == '<':
        return v < r
    if op == '=':
        return v[:3] == r[:3] and v[3] == r[3]
    if op == '~':
        up = (r[0], r[1] + 1, 0, (0, '')) if ncomp >= 2 else (r[0] + 1, 0, 0, (0, ''))
        return r <= v < up
    # caret
    if ncomp == 1:
        up = (r[0] + 1, 0, 0, (0, ''))
    elif ncomp == 2 and r[0] == 0:
        up = (0, r[1] + 1, 0, (0, ''))
    else:
        up = caret_upper(r)
    return r <= v < up


def matches(v, req):
    return all(matches_one(v, p) for p in req.split(','))


# registry: name -> {version_str: path}
local = {}
for d in os.listdir(reg):
    m = re.match(r'^(.+?)-(\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.+-]*)?)$', d)
    if m:
        local.setdefault(m.group(1), {})[m.group(2)] = os.path.join(reg, d)

pairs, singles, fixed_only, skipped = [], [], [], {'informational': 0, 'withdrawn': 0, 'no_local': 0}
for md in sorted(glob.glob(os.path.join(db, 'crates', '*', 'RUSTSEC-*.md'))):
    text = open(md, encoding='utf-8').read()
    fm = re.search(r'```toml\n(.*?)\n```', text, re.S)
    meta = tomllib.loads(fm.group(1))
    adv = meta['advisory']
    info = adv.get('informational')
    if adv.get('withdrawn'):
        skipped['withdrawn'] += 1
        continue
    if info in ('unmaintained', 'notice'):
        skipped['informational'] += 1
        continue
    name = adv['package']
    vers = local.get(name)
    if not vers:
        skipped['no_local'] += 1
        continue
    patched = meta.get('versions', {}).get('patched', [])
    unaffected = meta.get('versions', {}).get('unaffected', [])
    status = {}
    for vs in vers:
        v = parse_ver(vs)
        if v is None:
            continue
        if any(matches(v, r) for r in patched):
            status[vs] = 'patched'
        elif any(matches(v, r) for r in unaffected):
            status[vs] = 'unaffected'
        else:
            status[vs] = 'vulnerable'
    title = re.search(r'^# (.+)$', text, re.M)
    rec = {
        'id': adv['id'], 'crate': name, 'date': str(adv.get('date')), 'url': adv.get('url'),
        'aliases': adv.get('aliases', []), 'categories': adv.get('categories', []),
        'keywords': adv.get('keywords', []), 'informational': info, 'title': title.group(1) if title else None,
        'patched': patched, 'unaffected': unaffected,
        'affected_functions': meta.get('affected', {}).get('functions', {}),
        'references': adv.get('references', []),
        'local_versions': status, 'advisory_file': os.path.relpath(md, db),
    }
    vul = sorted((v for v, s in status.items() if s == 'vulnerable'), key=parse_ver)
    fix = sorted((v for v, s in status.items() if s == 'patched'), key=parse_ver)
    best = None
    for p in fix:  # smallest patched version with a vulnerable version below it
        below = [v for v in vul if parse_ver(v) < parse_ver(p)]
        if below:
            best = (below[-1], p)
            break
    if best:
        rec['pair'] = {'vuln': best[0], 'fixed': best[1], 'vuln_path': vers[best[0]], 'fixed_path': vers[best[1]]}
        pairs.append(rec)
    elif vul:
        singles.append(rec)
    else:
        fixed_only.append(rec)

json.dump({'pairs': pairs, 'singles': singles, 'fixed_only': fixed_only, 'skipped': skipped}, open(outp, 'w'), indent=1)
print(len(pairs), 'pairs;', len(singles), 'singles;', len(fixed_only), 'patched-only;', skipped)

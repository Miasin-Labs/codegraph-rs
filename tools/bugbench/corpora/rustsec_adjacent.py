"""Adjacent-version RustSec pairs from crates.io (public downloads).

For each non-informational, non-withdrawn advisory with a `patched` range:
the smallest published (non-yanked, non-prerelease) patched version p whose
immediate predecessor v is vulnerable -> pair (v, p). Prefer the local cargo
cache copy when present (read-only), else download the .crate from
static.crates.io into a private cache. Also fetch the advisory's fix diff
(GitHub commit/PR .diff) when its url/references name one.

usage: rustsec_adjacent.py <advisory-db> <registry-src> <crate-cache> <fixdiff-cache> <out-pairs.json>
"""
import glob
import io
import json
import os
import re
import sys
import tarfile
import tomllib
import urllib.request
from concurrent.futures import ThreadPoolExecutor

sys.path.insert(0, os.path.dirname(__file__))
import importlib.util  # noqa: E402

db, reg, cache, fixcache, outp = sys.argv[1:6]
os.makedirs(cache, exist_ok=True)
os.makedirs(fixcache, exist_ok=True)

# reuse semver helpers from rustsec_pairs without running it
src = open(os.path.join(os.path.dirname(__file__), 'rustsec_pairs.py')).read().split('# registry:')[0]
ns = {}
exec(compile(src.replace('db, reg, outp = sys.argv[1:4]', ''), 'rustsec_pairs', 'exec'), ns)
parse_ver, matches = ns['parse_ver'], ns['matches']

UA = {'User-Agent': 'codegraph-bench-corpus (research benchmark corpus build)'}
MAX_CRATE = 30 * 1024 * 1024


def get(url, limit=None):
    req = urllib.request.Request(url, headers=UA)
    with urllib.request.urlopen(req, timeout=60) as r:
        data = r.read(limit + 1 if limit else -1)
        if limit and len(data) > limit:
            raise ValueError('too large')
        return data


def index_path(name):
    n = name.lower()
    if len(n) <= 2:
        return f'{len(n)}/{n}'
    if len(n) == 3:
        return f'3/{n[0]}/{n}'
    return f'{n[:2]}/{n[2:4]}/{n}'


def versions(name):
    raw = get(f'https://index.crates.io/{index_path(name)}').decode()
    vs = []
    for ln in raw.splitlines():
        if ln.strip():
            j = json.loads(ln)
            vs.append((j['vers'], j.get('yanked', False)))
    return vs


def local_dir(name, ver):
    d = os.path.join(reg, f'{name}-{ver}')
    return d if os.path.isdir(d) else None


import threading
_locks = {}
_glock = threading.Lock()


def fetch_crate(name, ver):
    with _glock:
        lk = _locks.setdefault((name, ver), threading.Lock())
    with lk:
        return _fetch_crate(name, ver)


def _fetch_crate(name, ver):
    d = os.path.join(cache, f'{name}-{ver}')
    if os.path.isdir(d):
        return d
    data = get(f'https://static.crates.io/crates/{name}/{name}-{ver}.crate', MAX_CRATE)
    tmp = d + '.tmp'
    with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as tf:
        members = []
        for m in tf.getmembers():
            parts = m.name.split('/', 1)
            if len(parts) < 2 or not (m.isfile() or m.isdir()):
                continue
            rel = parts[1]
            if rel in ('Cargo.toml', 'Cargo.toml.orig', 'build.rs') or rel.startswith('src/'):
                m.name = rel
                members.append(m)
        tf.extractall(tmp, members=members, filter='data')
    os.rename(tmp, d)
    return d


import subprocess
import time
_ghlock = threading.Lock()


def gh_diff(u):
    m = re.match(r'https://github\.com/([^/]+/[^/]+)/(commit|pull)/(\w+)\.diff', u)
    repo, kind, ref = m.groups()
    ep = f'repos/{repo}/commits/{ref}' if kind == 'commit' else f'repos/{repo}/pulls/{ref}'
    with _ghlock:
        for attempt in range(3):
            p = subprocess.run(['gh', 'api', ep, '-H', 'Accept: application/vnd.github.diff'], capture_output=True, timeout=120)
            if p.returncode == 0:
                return p.stdout
            if b'rate limit' in p.stderr.lower():
                time.sleep(30)
                continue
            break
    raise RuntimeError(p.stderr.decode(errors='replace')[:300])


def fix_urls(adv):
    urls = [adv.get('url') or ''] + list(adv.get('references', []))
    out = []
    for u in urls:
        m = re.match(r'https://github\.com/([^/]+/[^/]+)/(commit|pull)/([0-9a-f]{7,40}|\d+)', u)
        if m:
            out.append(f'https://github.com/{m.group(1)}/{m.group(2)}/{m.group(3)}.diff')
    return out


def fetch_fix(aid, urls):
    got = []
    for i, u in enumerate(urls):
        p = os.path.join(fixcache, f'{aid}-{i}.diff')
        if not os.path.exists(p):
            try:
                data = gh_diff(u)
                open(p, 'wb').write(data)
            except Exception as e:  # noqa: BLE001
                open(p + '.err', 'w').write(str(e))
                continue
        got.append({'url': u, 'path': p})
    return got


def process(md):
    text = open(md, encoding='utf-8').read()
    meta = tomllib.loads(re.search(r'```toml\n(.*?)\n```', text, re.S).group(1))
    adv = meta['advisory']
    info = adv.get('informational')
    if adv.get('withdrawn') or info in ('unmaintained', 'notice'):
        return None
    patched = meta.get('versions', {}).get('patched', [])
    unaffected = meta.get('versions', {}).get('unaffected', [])
    name = adv['package']
    title = re.search(r'^# (.+)$', text, re.M)
    rec = {
        'id': adv['id'], 'crate': name, 'date': str(adv.get('date')), 'url': adv.get('url'),
        'aliases': adv.get('aliases', []), 'categories': adv.get('categories', []),
        'keywords': adv.get('keywords', []), 'informational': info, 'title': title.group(1) if title else None,
        'patched': patched, 'unaffected': unaffected,
        'affected_functions': meta.get('affected', {}).get('functions', {}),
        'references': adv.get('references', []), 'advisory_file': os.path.relpath(md, db),
    }
    if not patched:
        rec['skip'] = 'no patched version'
        return rec
    try:
        vs = versions(name)
    except Exception as e:  # noqa: BLE001
        rec['skip'] = f'index: {e}'
        return rec
    pub = sorted(((v, parse_ver(v)) for v, y in vs if not y and parse_ver(v) and '-' not in v.split('+')[0]), key=lambda t: t[1])

    def status(pv):
        if any(matches(pv, r) for r in patched):
            return 'patched'
        if any(matches(pv, r) for r in unaffected):
            return 'unaffected'
        return 'vulnerable'

    pair = None
    for i in range(1, len(pub)):
        if status(pub[i][1]) == 'patched' and status(pub[i - 1][1]) == 'vulnerable':
            pair = (pub[i - 1][0], pub[i][0])
            break
    if not pair:
        rec['skip'] = 'no vulnerable->patched adjacent published versions'
        return rec
    paths, srcs = [], []
    for v in pair:
        ld = local_dir(name, v)
        if ld:
            paths.append(ld)
            srcs.append('local-cargo-cache')
            continue
        try:
            paths.append(fetch_crate(name, v))
            srcs.append('crates.io')
        except Exception as e:  # noqa: BLE001
            rec['skip'] = f'download {name}-{v}: {e}'
            return rec
    def srcsize(d):
        return sum(os.path.getsize(os.path.join(a, f)) for a, _, fs in os.walk(os.path.join(d, 'src')) for f in fs)
    if max(srcsize(p) for p in paths) > 40 * 1024 * 1024:
        rec['skip'] = 'library src > 40 MiB (generated bindings)'
        return rec
    rec['pair'] = {'vuln': pair[0], 'fixed': pair[1], 'vuln_path': paths[0], 'fixed_path': paths[1],
                   'vuln_source': srcs[0], 'fixed_source': srcs[1], 'adjacent': True}
    rec['fix_diffs'] = fetch_fix(adv['id'], fix_urls(adv))
    return rec


mds = sorted(glob.glob(os.path.join(db, 'crates', '*', 'RUSTSEC-*.md')))
with ThreadPoolExecutor(12) as ex:
    res = [r for r in ex.map(process, mds) if r]
pairs = [r for r in res if 'pair' in r]
skipped = [{'id': r['id'], 'crate': r['crate'], 'reason': r['skip']} for r in res if 'skip' in r]
json.dump(pairs, open(outp, 'w'), indent=1)
json.dump(skipped, open(outp.replace('.json', '_skipped.json'), 'w'), indent=1)
print(len(pairs), 'pairs', len(skipped), 'skipped')

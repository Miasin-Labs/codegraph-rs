"""Materialize RustSec vuln/fixed pairs, diff their library sources, and
write meta.json per advisory + ground_truth.jsonl for the corpus.

usage: rustsec_build.py <pairs.json> <out_corpus_dir> <corpus_name>
pairs.json: list of advisory records (rustsec_pairs.py shape) each with
  pair = {vuln, fixed, vuln_path, fixed_path, vuln_source, fixed_source}
Sources are copied (never symlinked) with shutil.copytree; the registry is
only read.
"""
import json
import os
import re
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(__file__))
from rsfuncs import enclosing, functions  # noqa: E402

pairs_path, out_dir, corpus = sys.argv[1:4]
recs = json.load(open(pairs_path))
os.makedirs(out_dir, exist_ok=True)
IGN = shutil.ignore_patterns('target', '.git', '*.png', '*.jpg', '*.gif', '*.pdf', '*.wasm', '*.so', '*.a', '*.dll')


def copy_lib(src, dst):
    # library sources only: Cargo manifest, build script, src/ (keeps disk small)
    os.makedirs(dst)
    for f in ('Cargo.toml', 'Cargo.toml.orig', 'build.rs'):
        if os.path.isfile(os.path.join(src, f)):
            shutil.copy2(os.path.join(src, f), dst)
    if os.path.isdir(os.path.join(src, 'src')):
        shutil.copytree(os.path.join(src, 'src'), os.path.join(dst, 'src'), ignore=IGN, symlinks=False)
    else:
        shutil.copytree(src, dst, ignore=IGN, symlinks=False, dirs_exist_ok=True)


def lib_root(d):
    return 'src' if os.path.isdir(os.path.join(d, 'src')) else '.'


STOP = {'self', 'mut', 'let', 'std', 'core', 'alloc', 'unsafe', 'true', 'false', 'none', 'some', 'ok', 'err',
        'new', 'from', 'into', 'default', 'clone', 'drop', 'unwrap', 'len', 'the', 'and', 'use', 'crate', 'impl', 'fn',
        'usize', 'isize', 'u8', 'u16', 'u32', 'u64', 'u128', 'i8', 'i16', 'i32', 'i64', 'i128', 'bool', 'str', 'char',
        'f32', 'f64', 'panic', 'unwind', 'send', 'sync', 'sized'}


def names_of_interest(rec):
    names = set()
    for path in rec.get('affected_functions') or {}:
        names.add(path.split('::')[-1])
    body = re.sub(r'```.*?```', ' ', rec.get('_body', ''), flags=re.S)
    for span in re.findall(r'(?<!`)`([^`\n]+)`(?!`)', (rec.get('title') or '') + ' ' + body):
        span = re.sub(r'\(.*$', '', span.strip())
        last = re.split(r'::|\.', span)[-1].strip()
        if re.match(r'^[A-Za-z_][A-Za-z0-9_]*$', last) and len(last) > 2 and last.lower() not in STOP and not last[0].isupper():
            names.add(last)
    return names


def hunks(vdir, fdir, sub):
    adir = os.path.dirname(vdir)
    p = subprocess.run(['git', 'diff', '--no-index', '--no-color', '-U0', '--no-renames',
                        os.path.join('vuln', sub), os.path.join('fixed', sub)], capture_output=True, text=True,
                       errors='replace', cwd=adir)
    out, cur = [], None
    for ln in p.stdout.splitlines():
        if ln.startswith('diff --git'):
            cur = None
        elif ln.startswith('--- '):
            a = ln[4:]
            cur = {'a': None if a == '/dev/null' else os.path.relpath(a[2:] if a.startswith('a/') else a, 'vuln')}
        elif ln.startswith('+++ ') and cur is not None:
            b = ln[4:]
            cur['b'] = None if b == '/dev/null' else os.path.relpath(b[2:] if b.startswith('b/') else b, 'fixed')
        elif ln.startswith('@@') and cur is not None:
            m = re.match(r'@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@', ln)
            a0, al, b0, bl = int(m.group(1)), int(m.group(2) or 1), int(m.group(3)), int(m.group(4) or 1)
            out.append({'vuln_file': cur['a'], 'fixed_file': cur['b'], 'vuln_start': a0, 'vuln_len': al,
                        'fixed_start': b0, 'fixed_len': bl, 'added': [], 'removed': []})
        elif out and ln[:1] in '+-' and not ln.startswith(('+++', '---')):
            out[-1]['added' if ln[0] == '+' else 'removed'].append(ln[1:][:200])
    return out


def load_fix(aid):
    """(basenames, removed-lines, added-lines) of the advisory's fetched fix diffs"""
    d = os.environ.get('FIXDIFFS')
    names, rem, add, urls = set(), set(), set(), []
    if not d:
        return names, rem, add, urls
    for f in sorted(os.listdir(d)):
        if not (f.startswith(aid + '-') and f.endswith('.diff')):
            continue
        urls.append(f)
        cur = None
        for ln in open(os.path.join(d, f), encoding='utf-8', errors='replace'):
            if ln.startswith('+++ ') or ln.startswith('--- '):
                p = ln[4:].strip()
                if p != '/dev/null':
                    cur = p.split('/', 1)[-1]
                    if cur.endswith('.rs') and '/tests/' not in '/' + cur:
                        names.add(os.path.basename(cur))
                continue
            if not cur or not cur.endswith('.rs') or '/tests/' in '/' + cur:
                continue
            t = ln[1:].strip()
            if len(t) < 4 or t in ('} else {', '});', '})', 'Ok(())'):
                continue
            if ln.startswith('-'):
                rem.add(t)
            elif ln.startswith('+'):
                add.add(t)
    return names, rem, add, urls


_fcache = {}


def funcs_of(path):
    if path not in _fcache:
        try:
            _fcache[path] = functions(open(path, encoding='utf-8', errors='replace').read())
        except OSError:
            _fcache[path] = []
    return _fcache[path]


gt = open(os.path.join(out_dir, 'ground_truth.jsonl'), 'w')
summary = []
for rec in recs:
    pr = rec['pair']
    body = open(os.path.join(os.environ['ADVDB'], rec['advisory_file']), encoding='utf-8').read()
    rec['_body'] = body.split('```', 2)[-1]
    adir = os.path.join(out_dir, rec['id'])
    vdir, fdir = os.path.join(adir, 'vuln'), os.path.join(adir, 'fixed')
    for src, dst in ((pr['vuln_path'], vdir), (pr['fixed_path'], fdir)):
        if not os.path.isdir(dst):
            copy_lib(src, dst)
    sub = lib_root(vdir)
    hs = hunks(vdir, fdir, sub)
    hs = [h for h in hs if (h['vuln_file'] or h['fixed_file'] or '').endswith('.rs')]
    interest = names_of_interest(rec)
    fix_names, fix_rem, fix_add, fix_files = load_fix(rec['id'])
    files = set()
    for h in hs:
        vf = enclosing(funcs_of(os.path.join(vdir, h['vuln_file'])), h['vuln_start'], h['vuln_start'] + max(h['vuln_len'], 1) - 1) if h['vuln_file'] else None
        ff = enclosing(funcs_of(os.path.join(fdir, h['fixed_file'])), h['fixed_start'], h['fixed_start'] + max(h['fixed_len'], 1) - 1) if h['fixed_file'] else None
        h['vuln_fn'] = vf
        h['fixed_fn'] = ff
        text = ' '.join(h['added'] + h['removed'])
        h['mentions_interest'] = sorted(n for n in interest if re.search(r'\b%s\b' % re.escape(n), text)
                                        or (vf and vf['name'] == n) or (ff and ff['name'] == n))
        base_n = os.path.basename(h['vuln_file'] or h['fixed_file'])
        ov = sum(1 for t in h['removed'] if t.strip() in fix_rem) + sum(1 for t in h['added'] if t.strip() in fix_add)
        h['fix_commit_overlap'] = ov if base_n in fix_names else 0
        files.add(h['vuln_file'] or h['fixed_file'])
    added = sum(len(h['added']) for h in hs)
    removed = sum(len(h['removed']) for h in hs)
    changed = added + removed
    tight = 'tight' if changed <= 200 and len(files) <= 5 else 'medium' if changed <= 2000 else 'loose'
    meta = {k: rec.get(k) for k in ('id', 'crate', 'date', 'title', 'url', 'aliases', 'categories', 'keywords',
                                    'informational', 'patched', 'unaffected', 'affected_functions', 'references', 'advisory_file')}
    meta.update({
        'vuln_version': pr['vuln'], 'fixed_version': pr['fixed'],
        'vuln_source': pr.get('vuln_source', 'local-cargo-cache'), 'fixed_source': pr.get('fixed_source', 'local-cargo-cache'),
        'adjacent_versions': pr.get('adjacent'),
        'lib_root': sub, 'diff_files': len(files), 'diff_lines_added': added, 'diff_lines_removed': removed,
        'diff_tightness': tight, 'names_of_interest': sorted(interest),
        'fix_diffs': fix_files, 'fix_commit_localized_hunks': sum(1 for h in hs if h['fix_commit_overlap']),
        'hunks': [{k: v for k, v in h.items() if k not in ('added', 'removed')} | {'added': h['added'][:40], 'removed': h['removed'][:40]} for h in hs[:400]],
        'hunks_truncated': len(hs) > 400,
    })
    json.dump(meta, open(os.path.join(adir, 'meta.json'), 'w'), indent=1)
    cats = rec.get('categories') or []
    base = {'corpus': corpus, 'advisory': rec['id'], 'crate': rec['crate'], 'cwe': None,
            'category': ','.join(cats) or rec.get('informational') or None, 'diff_tightness': tight,
            'source_of_truth': 'RustSec advisory + vuln->fixed library diff'}
    seen = set()
    for h in hs:
        fnv = h['vuln_fn'] or h['fixed_fn']
        in_test = bool(fnv and (fnv['owner'] or '').startswith('mod test')) or '/tests/' in '/' + (h['vuln_file'] or h['fixed_file'])
        rel = 'test' if in_test else 'fix_commit' if h['fix_commit_overlap'] else 'name_match' if h['mentions_interest'] else 'diff'
        for side, lab, f, fn, s, n in (('vuln', 'bad', h['vuln_file'], h['vuln_fn'], h['vuln_start'], h['vuln_len']),
                                       ('fixed', 'good', h['fixed_file'], h['fixed_fn'], h['fixed_start'], h['fixed_len'])):
            if not f:
                continue
            path = f'{rec["id"]}/{side}/{f}'
            gt.write(json.dumps(base | {'file': path, 'line_start': s, 'line_end': s + max(n, 1) - 1, 'label': lab,
                                        'granularity': 'line', 'relevance': rel,
                                        'function': (fn['owner'] + '::' if fn and fn['owner'] else '') + fn['name'] if fn else None}) + '\n')
            if fn and (path, fn['line_start']) not in seen:
                seen.add((path, fn['line_start']))
                gt.write(json.dumps(base | {'file': path, 'function': (fn['owner'] + '::' if fn['owner'] else '') + fn['name'],
                                            'line_start': fn['line_start'], 'line_end': fn['line_end'], 'label': lab,
                                            'granularity': 'function', 'relevance': rel}) + '\n')
    summary.append((rec['id'], rec['crate'], pr['vuln'], pr['fixed'], len(files), changed, tight,
                    sum(1 for h in hs if h['mentions_interest']), sum(1 for h in hs if h['fix_commit_overlap'])))
    print(*summary[-1], flush=True)
json.dump(summary, open(os.path.join(out_dir, 'summary.json'), 'w'), indent=0)

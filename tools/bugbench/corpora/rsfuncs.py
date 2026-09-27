"""Lexical Rust fn locator: blanks comments/strings/chars, then for each
`fn name` finds its body braces. Returns [{name, owner, line_start, line_end}],
owner = the innermost enclosing `impl ... {` / `trait X {` / `mod x {` header
(compact text), or None. Good enough to name the enclosing fn of a diff hunk.
"""
import bisect
import re

_TOK = re.compile(
    r'//[^\n]*'
    r'|/\*.*?\*/'
    r'|b?r(#*)".*?"\1'
    r'|b?"(?:\\.|[^"\\])*"'
    r"|b?'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]+\}|.)|[^\\'\n])'",
    re.S,
)


def _blank(m):
    return re.sub(r'[^\n]', ' ', m.group(0))


def functions(text):
    clean = _TOK.sub(_blank, text)
    nl = [i for i, c in enumerate(clean) if c == '\n']

    def line(p):
        return bisect.bisect_left(nl, p) + 1

    # match every brace pair
    pair = {}
    st = []
    for m in re.finditer(r'[{}]', clean):
        if m.group(0) == '{':
            st.append(m.start())
        elif st:
            pair[st.pop()] = m.start()
    # owners: impl/trait/mod blocks
    owners = []
    for m in re.finditer(r'\b(impl|trait|mod)\b', clean):
        j = clean.find('{', m.end())
        k = clean.find(';', m.end())
        if j < 0 or (0 <= k < j) or j not in pair:
            continue
        hdr = ' '.join(clean[m.start():j].split())
        if len(hdr) > 160:
            hdr = hdr[:157] + '...'
        owners.append((j, pair[j], hdr))
    out = []
    for m in re.finditer(r'\bfn\s+([A-Za-z_]\w*)', clean):
        # find body `{` or `;` at paren depth 0
        depth = 0
        i = m.end()
        body = None
        while i < len(clean):
            c = clean[i]
            if c in '([':
                depth += 1
            elif c in ')]':
                depth -= 1
            elif c == ';' and depth == 0:
                break
            elif c == '{' and depth == 0:
                body = i
                break
            i += 1
        if body is None or body not in pair:
            continue
        own = [o for o in owners if o[0] < m.start() < o[1]]
        own = max(own, key=lambda o: o[0])[2] if own else None
        out.append({'name': m.group(1), 'owner': own, 'line_start': line(m.start()), 'line_end': line(pair[body])})
    return out


def enclosing(funcs, a, b):
    """innermost fn overlapping [a, b]"""
    c = [f for f in funcs if f['line_start'] <= b and a <= f['line_end']]
    return min(c, key=lambda f: f['line_end'] - f['line_start']) if c else None

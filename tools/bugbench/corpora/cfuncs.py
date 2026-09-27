"""Tiny brace-matching function locator for C / C++ / Java (and similar).

No tree-sitter available here; this is a lexical heuristic tuned for the very
regular layout of Juliet and OWASP Benchmark code. It blanks comments, string
and char literals and preprocessor lines, then walks braces keeping a scope
stack (container = namespace/class/struct/extern, function, block).

functions(text) -> list of dict(name, qualified, line_start, line_end)
"""
import bisect
import re

_TOK = re.compile(
    r'//[^\n]*'
    r'|/\*.*?\*/'
    r'|"(?:\\.|[^"\\\n])*"'
    r"|'(?:\\.|[^'\\\n])*'"
    r'|(?m:^[ \t]*#(?:[^\n]*\\\n)*[^\n]*)',
    re.S,
)
_CONTAINER = re.compile(r'\b(namespace|class|struct|union|enum|interface|record)\b[^()]*$|\bextern\s*$')
_ANNOT = re.compile(r'@\w+(?:\s*\([^()]*\))?')
_TRAIL = re.compile(r'(\s*(const|override|final|noexcept|throws\s+[\w.,\s]+))+\s*$')
_NAME = re.compile(r'([~\w:]+|operator\s*\S+)\s*$')
_KEYWORDS = {'if', 'for', 'while', 'switch', 'catch', 'return', 'sizeof', 'else', 'do', 'synchronized', 'try'}


def _blank(m):
    s = m.group(0)
    return re.sub(r'[^\n]', ' ', s)


def functions(text):
    clean = _TOK.sub(_blank, text)
    nl = [i for i, c in enumerate(clean) if c == '\n']

    def line(pos):
        return bisect.bisect_left(nl, pos) + 1

    out = []
    stack = []  # entries: ('container'|'func'|'block', info)
    seg = 0
    for m in re.finditer(r'[{};]', clean):
        c = m.group(0)
        p = m.start()
        if c == ';':
            seg = p + 1
            continue
        if c == '}':
            if stack:
                kind, info = stack.pop()
                if kind == 'func':
                    info['line_end'] = line(p)
                    out.append(info)
            seg = p + 1
            continue
        # '{'
        header_raw = clean[seg:p]
        header = _ANNOT.sub(' ', header_raw)
        header_s = header.strip()
        at_container = all(k == 'container' for k, _ in stack)
        kind = 'block'
        info = None
        if at_container:
            if _CONTAINER.search(header_s) and '(' not in header_s:
                kind = 'container'
                cm = re.search(r'\b(?:namespace|class|struct|union|enum|interface|record)\s+([\w:]+)', header_s)
                info = cm.group(1) if cm else ''
            else:
                h = _TRAIL.sub('', header_s)
                if h.endswith(')') and '(' in h:
                    first = h.index('(')
                    nm = _NAME.search(h[:first])
                    if nm and nm.group(1).split('::')[-1] not in _KEYWORDS and '=' not in h[:first]:
                        name = nm.group(1).strip()
                        idx = header.rfind(name, 0, header.index('(') + 1 if '(' in header else len(header))
                        npos = seg + (idx if idx >= 0 else len(header_raw) - len(header_raw.lstrip()))
                        quals = [i for k, i in stack if k == 'container' and i]
                        kind = 'func'
                        info = {
                            'name': name.split('::')[-1],
                            'qualified': '::'.join(quals + [name]) if quals else name,
                            'line_start': line(npos),
                            'body_start': line(p),
                        }
        stack.append((kind, info))
        seg = p + 1
    out.sort(key=lambda f: f['line_start'])
    return out


if __name__ == '__main__':
    import json
    import sys

    for f in functions(open(sys.argv[1], encoding='latin-1').read()):
        print(json.dumps(f))

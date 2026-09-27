"""Ground truth for the deliberately vulnerable web apps under bench/webapps.

juice-shop : `vuln-code-snippet vuln-line <challenge>` markers in the live
             source (line-level, bad) + data/static/codefixes snippet variants
             (<challenge>_N.ts = bad, <challenge>_N_correct.ts = good; file-level).
DVWA       : vulnerabilities/<vuln>/source/{low,medium,high}.php = bad,
             impossible.php = good (file-level, by the project's own design).
NodeGoat   : hand-curated from the "Fix for A.." comments (line-level, bad).
pygoat     : hand-curated lab functions in introduction/views.py (function-level
             with sink line where there is one).
Vulnerable-Flask-App : hand-curated from app/app.py (line/function-level).
"""
import ast
import json
import os
import re
import sys

import yaml

W = sys.argv[1]  # bench/webapps

JUICE_CWE = {
    'XSS': 'CWE-79', 'Injection': 'CWE-89', 'Broken Access Control': 'CWE-284',
    'Improper Input Validation': 'CWE-20', 'Sensitive Data Exposure': 'CWE-200',
    'Broken Authentication': 'CWE-287', 'Security Misconfiguration': 'CWE-16',
    'Cryptographic Issues': 'CWE-327', 'Unvalidated Redirects': 'CWE-601',
    'XXE': 'CWE-611', 'Insecure Deserialization': 'CWE-502',
    'Vulnerable Components': 'CWE-1104', 'Broken Anti Automation': 'CWE-799',
    'Security through Obscurity': 'CWE-656', 'Observability Failures': 'CWE-778',
    'Miscellaneous': None,
}


def w(fh, **rec):
    fh.write(json.dumps({k: v for k, v in rec.items() if v is not None}) + '\n')


def juice():
    root = os.path.join(W, 'juice-shop')
    chal = {c['key']: c for c in yaml.safe_load(open(os.path.join(root, 'data/static/challenges.yml')))}
    out = open(os.path.join(root, 'ground_truth.jsonl'), 'w')
    n = {'bad': 0, 'good': 0}
    skip = {'node_modules', '.git', '.ai', 'codefixes'}
    for d, ds, fs in os.walk(root):
        ds[:] = [x for x in ds if x not in skip]
        for f in fs:
            p = os.path.join(d, f)
            rel = os.path.relpath(p, root)
            if rel == 'lib/codingChallenges.ts' or rel.startswith('test/'):
                continue
            try:
                lines = open(p, encoding='utf-8').read().split('\n')
            except (UnicodeDecodeError, OSError):
                continue
            starts = {}
            for i, ln in enumerate(lines, 1):
                m = re.search(r'vuln-code-snippet start (.*)$', ln)
                if m:
                    for k in m.group(1).split():
                        starts[k] = i
            for i, ln in enumerate(lines, 1):
                m = re.search(r'vuln-code-snippet vuln-line (.*)$', ln)
                if not m:
                    continue
                for k in m.group(1).split():
                    end = next((j for j, l2 in enumerate(lines, 1) if re.search(r'vuln-code-snippet end .*\b%s\b' % k, l2)), None)
                    cat = chal.get(k, {}).get('category')
                    w(out, corpus='juice-shop', file=rel, line_start=i, line_end=i, cwe=JUICE_CWE.get(cat),
                      category=cat, challenge=k, snippet_start=starts.get(k), snippet_end=end,
                      label='bad', granularity='line', source_of_truth='vuln-code-snippet marker + challenges.yml')
                    n['bad'] += 1
    cf = os.path.join(root, 'data/static/codefixes')
    for f in sorted(os.listdir(cf)):
        m = re.match(r'(\w+?)_(\d+)(_correct)?\.(ts|js|yml|sol|tf)$', f)
        if not m:
            continue
        k = m.group(1)
        cat = chal.get(k, {}).get('category')
        lab = 'good' if m.group(3) else 'bad'
        n[lab] += 1
        w(out, corpus='juice-shop', file=f'data/static/codefixes/{f}', cwe=JUICE_CWE.get(cat), category=cat,
          challenge=k, label=lab, granularity='file', subset='codefixes',
          source_of_truth='codefixes naming (_correct = fixed variant); snippet, not compiled code')
    return n


DVWA_CWE = {
    'api': ('CWE-639', 'API broken object/property auth'), 'authbypass': ('CWE-285', 'Authorisation bypass'),
    'bac': ('CWE-639', 'Broken access control'), 'brute': ('CWE-307', 'Brute force'),
    'captcha': ('CWE-804', 'Insecure CAPTCHA'), 'cryptography': ('CWE-327', 'Cryptography'),
    'csp': ('CWE-693', 'CSP bypass'), 'csrf': ('CWE-352', 'CSRF'), 'exec': ('CWE-78', 'Command injection'),
    'fi': ('CWE-98', 'File inclusion'), 'javascript': ('CWE-602', 'Client-side security'),
    'open_redirect': ('CWE-601', 'Open redirect'), 'sqli': ('CWE-89', 'SQL injection'),
    'sqli_blind': ('CWE-89', 'Blind SQL injection'), 'upload': ('CWE-434', 'File upload'),
    'weak_id': ('CWE-330', 'Weak session IDs'), 'xss_d': ('CWE-79', 'DOM XSS'),
    'xss_r': ('CWE-79', 'Reflected XSS'), 'xss_s': ('CWE-79', 'Stored XSS'),
}


def dvwa():
    root = os.path.join(W, 'DVWA')
    out = open(os.path.join(root, 'ground_truth.jsonl'), 'w')
    n = {'bad': 0, 'good': 0}
    for v in sorted(os.listdir(os.path.join(root, 'vulnerabilities'))):
        src = os.path.join(root, 'vulnerabilities', v, 'source')
        if not os.path.isdir(src) or v not in DVWA_CWE:
            continue
        cwe, cat = DVWA_CWE[v]
        for f in sorted(os.listdir(src)):
            m = re.match(r'(low|medium|high|impossible)\.(php|js)$', f)
            if not m:
                continue
            lab = 'good' if m.group(1) == 'impossible' else 'bad'
            n[lab] += 1
            w(out, corpus='dvwa', file=f'vulnerabilities/{v}/source/{f}', cwe=cwe, category=cat,
              security_level=m.group(1), label=lab, granularity='file',
              source_of_truth='DVWA design: low/medium/high are exploitable, impossible is the secure reference')
    return n


NODEGOAT = [
    ('app/routes/contributions.js', 32, 34, 'handleContributionsUpdate', 'CWE-95', 'A1 SSJS injection (eval of req.body)'),
    ('app/data/allocations-dao.js', 77, 79, 'getByUserIdAndThreshold', 'CWE-943', 'A1 NoSQL injection ($where with threshold)'),
    ('app/routes/session.js', 64, 64, 'handleLoginRequest', 'CWE-117', 'A1 log injection'),
    ('app/routes/session.js', 85, 85, 'handleLoginRequest', 'CWE-204', 'A2 user enumeration via distinct error messages'),
    ('app/routes/session.js', 94, 94, 'handleLoginRequest', 'CWE-204', 'A2 user enumeration via distinct error messages'),
    ('app/routes/session.js', 116, 116, 'handleLoginRequest', 'CWE-384', 'A2 session fixation (no regenerate on login)'),
    ('app/routes/session.js', 144, 144, 'validateSignup', 'CWE-521', 'A2 weak password policy'),
    ('app/data/user-dao.js', 25, 25, 'addUser', 'CWE-256', 'A2 plaintext password storage'),
    ('app/data/user-dao.js', 61, 61, 'comparePassword', 'CWE-256', 'A2 plaintext password comparison'),
    ('app/routes/allocations.js', 16, 18, 'displayAllocations', 'CWE-639', 'A4 IDOR (userId from URL param)'),
    ('app/routes/index.js', 55, 56, None, 'CWE-285', 'A7 missing function-level access control on /benefits'),
    ('app/routes/index.js', 72, 72, None, 'CWE-601', 'A10 open redirect (/learn?url=)'),
    ('app/routes/research.js', 15, 16, 'displayResearch', 'CWE-918', 'SSRF (needle.get of req.query.url)'),
    ('app/routes/profile.js', 59, 59, 'handleProfileUpdate', 'CWE-1333', 'ReDoS regex'),
    ('app/data/profile-dao.js', 61, 66, 'updateUser', 'CWE-312', 'A6 cleartext storage of SSN/DOB'),
    ('server.js', 78, 102, None, 'CWE-1004', 'A3/A5 session cookie without httpOnly, default name'),
    ('server.js', 137, 137, None, 'CWE-79', 'A3 swig autoescape disabled'),
    ('server.js', 145, 145, None, 'CWE-319', 'A6 plain HTTP server'),
    ('server.js', 15, 157, None, 'CWE-352', 'A8 no CSRF middleware (absence; file-level)'),
    ('app/views/memos.html', 31, 31, None, 'CWE-79', 'stored XSS via marked(memo) with autoescape off'),
]

PYGOAT = [  # (function, sink line or None, cwe, note)
    ('xss_lab', None, 'CWE-79', 'reflected XSS: query rendered with |safe in xss_lab.html:27'),
    ('xss_lab2', None, 'CWE-79', 'XSS: naive <script> strip; username|safe in xss_lab_2.html:20'),
    ('xss_lab3', None, 'CWE-79', 'XSS: alnum strip only (JSFuck payloads)'),
    ('sql_lab', 162, 'CWE-89', 'SQL injection via objects.raw(string-built query)'),
    ('insec_des_lab', 214, 'CWE-502', 'pickle.loads of cookie'),
    ('xxe_parse', 259, 'CWE-611', 'SAX parser with external general entities enabled'),
    ('auth_lab_signup', 291, 'CWE-614', 'cookie secure=False, userid as session'),
    ('auth_lab_login', 305, 'CWE-565', 'userid cookie used as authentication'),
    ('ba_lab', 363, 'CWE-565', 'admin flag in client cookie'),
    ('data_exp_lab', None, 'CWE-209', 'debug/error information exposure lab'),
    ('cmd_lab', 430, 'CWE-78', 'subprocess.Popen(shell=True) with user domain'),
    ('cmd_lab2', 460, 'CWE-95', 'eval of user input'),
    ('login_otp', None, 'CWE-640', 'broken OTP flow (bau lab)'),
    ('Otp', 501, 'CWE-565', 'email in cookie drives OTP verification'),
    ('a9_lab', 560, 'CWE-502', 'yaml.load with yaml.Loader on uploaded file'),
    ('a9_lab2', 588, 'CWE-95', 'PIL ImageMath.eval with user expression (CVE-2022-22817)'),
    ('a10_lab', None, 'CWE-778', 'insufficient logging lab'),
    ('a10_lab2', None, 'CWE-778', 'insufficient logging lab'),
    ('insec_desgine_lab', None, 'CWE-840', 'insecure design (ticket business logic)'),
    ('a1_broken_access_lab_1', 774, 'CWE-565', 'admin cookie trusted'),
    ('a1_broken_access_lab_2', None, 'CWE-284', 'broken access control via User-Agent/role header'),
    ('a1_broken_access_lab_3', None, 'CWE-284', 'forced browsing to admin page'),
    ('injection_sql_lab', 878, 'CWE-89', 'SQL injection via objects.raw'),
    ('ssrf_lab', 927, 'CWE-22', 'user-controlled path opened (SSRF/LFI lab)'),
    ('ssrf_lab2', 963, 'CWE-918', 'requests.get(user url)'),
    ('ssti_lab', 995, 'CWE-1336', 'user blog written into a Django template'),
    ('crypto_failure_lab', 1026, 'CWE-328', 'unsalted md5 password hash'),
    ('crypto_failure_lab2', None, 'CWE-327', 'weak crypto lab'),
    ('crypto_failure_lab3', None, 'CWE-565', 'predictable/forgeable cookie'),
    ('sec_misconfig_lab3', 1095, 'CWE-798', 'JWT with hard-coded SECRET_COOKIE_KEY'),
    ('auth_failure_lab2', None, 'CWE-307', 'no brute-force protection'),
    ('auth_failure_lab3', None, 'CWE-916', 'sha256 unsalted password hash / session'),
    ('software_and_data_integrity_failure_lab2', None, 'CWE-494', 'integrity failure lab'),
    ('software_and_data_integrity_failure_lab3', None, 'CWE-494', 'integrity failure lab'),
]

FLASK = [  # (line_start, line_end, function, cwe, note)
    (26, 28, None, 'CWE-798', 'hard-coded HMAC/secret keys'),
    (63, 63, 'setup_users', 'CWE-798', 'hard-coded admin password'),
    (97, 97, 'insecure_verify', 'CWE-347', 'jwt.decode(verify=False)'),
    (112, 114, 'pnf', 'CWE-1336', 'SSTI: request.url into render_template_string'),
    (141, 141, 'reg_customer', 'CWE-328', 'md5 password hash'),
    (161, 161, 'reg_user', 'CWE-312', 'plaintext password / card number stored'),
    (182, 182, 'login', 'CWE-256', 'plaintext password comparison'),
    (208, 208, 'fetch_customer', 'CWE-639', 'IDOR: any customer id'),
    (227, 231, 'get_customer', 'CWE-639', 'IDOR behind unverified JWT'),
    (261, 265, 'search_customer', 'CWE-89', 'SQL injection via % formatting'),
    (280, 281, 'search_customer', 'CWE-1336', 'SSTI/info leak: exception text into render_template_string'),
    (303, 303, 'hello', 'CWE-611', 'XXE via python-docx on uploaded docx (library-dependent)'),
    (329, 329, 'yaml_hammer', 'CWE-502', 'yaml.load without SafeLoader'),
]


def pyfuncs(path):
    tree = ast.parse(open(path, encoding='utf-8').read())
    return {n.name: (n.lineno, n.end_lineno) for n in ast.walk(tree) if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))}


def nodegoat():
    root = os.path.join(W, 'NodeGoat')
    out = open(os.path.join(root, 'ground_truth.jsonl'), 'w')
    for f, a, b, fn, cwe, note in NODEGOAT:
        assert os.path.exists(os.path.join(root, f)), f
        w(out, corpus='nodegoat', file=f, function=fn, line_start=a, line_end=b, cwe=cwe, category=note,
          label='bad', granularity='line', source_of_truth='hand-curated from in-source "Fix for A.." comments + tutorial')
    return {'bad': len(NODEGOAT), 'good': 0}


def pygoat():
    root = os.path.join(W, 'pygoat')
    rel = 'introduction/views.py'
    fns = pyfuncs(os.path.join(root, rel))
    out = open(os.path.join(root, 'ground_truth.jsonl'), 'w')
    for fn, sink, cwe, note in PYGOAT:
        a, b = fns[fn]
        assert sink is None or a <= sink <= b, (fn, sink, a, b)
        w(out, corpus='pygoat', file=rel, function=fn, line_start=a, line_end=b, sink_line=sink, cwe=cwe,
          category=note, label='bad', granularity='line' if sink else 'function',
          source_of_truth='hand-curated from lab functions + Solutions/solution.md')
    return {'bad': len(PYGOAT), 'good': 0}


def flask():
    root = os.path.join(W, 'Vulnerable-Flask-App')
    rel = 'app/app.py'
    fns = pyfuncs(os.path.join(root, rel))
    out = open(os.path.join(root, 'ground_truth.jsonl'), 'w')
    for a, b, fn, cwe, note in FLASK:
        if fn:
            fa, fb = fns[fn]
            assert fa <= a and b <= fb, (fn, a, b, fa, fb)
        w(out, corpus='vulnerable-flask-app', file=rel, function=fn, line_start=a, line_end=b, cwe=cwe,
          category=note, label='bad', granularity='line', source_of_truth='hand-curated from app/app.py')
    return {'bad': len(FLASK), 'good': 0}


for name, fn in [('juice-shop', juice), ('dvwa', dvwa), ('nodegoat', nodegoat), ('pygoat', pygoat), ('flask', flask)]:
    print(name, fn())

# Bug/vulnerability detector benchmark corpora

Built 2026-09-27. Total on disk: **~1.9 GB**. Nothing here has been indexed with codegraph.
Generators live in `tools/`: `cfuncs.py` (C/C++/Java function locator), `rsfuncs.py` (Rust fn locator),
`juliet_gt.py`, `webapps_gt.py`, `rustsec_pairs.py`, `rustsec_adjacent.py`, `rustsec_build.py`, `rustsec_relabel.py`.

## Common ground-truth schema (`<corpus>/ground_truth.jsonl`, one JSON object per line)

| field | meaning |
|---|---|
| `corpus` | corpus name |
| `file` | path relative to the corpus directory (the dir holding the jsonl) |
| `function` | enclosing function / method (qualified with class/namespace/impl owner where known), optional |
| `line_start`, `line_end` | 1-based inclusive, optional (absent for pure file-level rows) |
| `cwe` | `CWE-N` or absent (RustSec/Rudra rows use `category` instead) |
| `category` | corpus-specific class (Juliet CWE dir, Benchmark category, RustSec categories, Rudra bug class, ...) |
| `label` | `bad` / `good` (plus Rust-only extras, below) |
| `granularity` | `function` / `file` / `line` |
| `source_of_truth` | where the label comes from |

Extra fields per corpus are documented below (e.g. Juliet `flaw_lines`, `role`; Rust `relevance`, `advisory`).
Line numbers for functions come from a lexical brace matcher (no tree-sitter available); spot-checked, not
parser-exact.

---

## 1. `juliet-c/` — NIST Juliet Test Suite for C/C++ v1.3
- Source: https://samate.nist.gov/SARD/downloads/test-suites/2017-10-01-juliet-test-suite-for-c-cplusplus-v1-3.zip (still at this URL; 152,957,342 bytes). Zip contents flattened (`C/` -> corpus root); `manifest.xml` kept.
- Languages: C, C++. 105,739 source files (54,484 .c, 46,747 .cpp, 4,496 .h), ~14.2 M lines (incl. `testcasesupport/`).
- Labels: **105,296 bad / 241,886 good functions**, 118 CWEs. Granularity **function** (+3 line-only rows).
  - label by Juliet role name: function name suffix after `_NN[a-z]_` containing `bad` -> bad (`bad`, `badSink`, `badSource`, `helperBad`, ...), containing `good` -> good (`good`, `goodG2B`, `goodB2GSink`, `good1`, ...). Class-based variants (`X_81_bad::action`, `*_goodG2B.cpp`) take the class/file role. `main` and unnamed helpers are skipped.
  - `flaw_lines`: the manifest's `<flaw line>` entries falling inside the function (64,153 bad functions carry one). 65,263 manifest flaws total; 3 fall outside any recorded function and are emitted as `granularity: line` rows.
  - `cwe` = the testcase directory's CWE.
- Caveats: a `bad` function is Juliet's *entry point* of a bad flow — for multi-file/`badSource`/`badSink` variants the flaw line may be in another function/file (score function hits on the flow, or use `flaw_lines` for line-exact scoring). `goodB2G` has a bad source and a good sink. Many testcases are Windows-only (`w32`, `wchar_t` APIs). The suite is synthetic and extremely regular — over-fits easily.

## 2. `juliet-java/` — NIST Juliet Test Suite for Java v1.3
- Source: https://samate.nist.gov/SARD/downloads/test-suites/2017-10-01-juliet-test-suite-for-java-v1-3.zip (76,798,417 bytes). Flattened; testcases under `src/testcases/`.
- Language: Java. 46,815 files, ~7.5 M lines.
- Labels: **47,121 bad / 129,380 good methods**, 112 CWEs, granularity **function**; same role rules and `flaw_lines` (25,609 bad methods carry one; 37,277 manifest flaws).
- Caveats: `manifest.xml` is not well-formed XML (a stray `</testcase>`), parsed with a regex. 12 `good` methods carry a manifest flaw line (class-level CWE-491/499/500 `*_good1.java` files — manifest quirk; treat as label conflicts). Needs servlet/`lib/` jars to compile, not needed for static analysis.

## 3. `owasp-benchmark-java/` — OWASP Benchmark v1.2 (Java)
- Source: https://github.com/OWASP-Benchmark/BenchmarkJava @ `20cbf3d11123347e47ed89541e6942836def53f7` (2026-09-08), shallow. Removed `.git`, `results/` (187 MB of third-party tool outputs), `scorecard/`, `VMs/`.
- Language: Java (servlets). 2,740 test cases in `src/main/java/org/owasp/benchmark/testcode/`; 5,522 files / ~388 K lines total.
- Labels (from `expectedresults-1.2.csv`): **1,415 true vulns / 1,325 false positives**, 11 CWEs across cmdi(78), crypto(327), hash(328), ldapi(90), pathtraver(22), securecookie(614), sqli(89), trustbound(501), weakrand(330), xpathi(643), xss(79). Granularity **file** (each row also names the `doPost` method and its lines).
- Caveats: the true/false distinction is often decided by dead branches, collection-index tricks, or config-driven sanitizers — built to punish pattern-only tools. Score with the Benchmark's own per-category TPR−FPR metric as well as P/R.

## 4. `webapps/` — deliberately vulnerable apps (realistic, sparse labels)
All shallow clones, `.git` removed.

| corpus dir | source @ commit | lang | files / LOC | labels | granularity | notes |
|---|---|---|---|---|---|---|
| `juice-shop/` | github.com/juice-shop/juice-shop @ `1618a611b173` (2026-08-10) | TS/JS (+yml, sol, tf) | 744 / ~109 K | 134 bad, 35 good | line + file | 44 rows = in-source `vuln-code-snippet vuln-line <challenge>` markers (with `snippet_start/end`); 125 rows = `data/static/codefixes/<challenge>_N.ts` variants (`_correct` = good). Category from `challenges.yml`, CWE hand-mapped from category (9 rows have none). |
| `NodeGoat/` | github.com/OWASP/NodeGoat @ `c5cb68a7084e` (2023-06-21) | JS | 68 / ~6 K | 20 bad | line | Hand-curated from the in-source `// Fix for A..` comments: eval SSJS, NoSQL `$where`, log injection, IDOR, open redirect, SSRF, ReDoS, plaintext passwords, session fixation, missing CSRF/httpOnly, autoescape off. |
| `DVWA/` | github.com/digininja/DVWA @ `b496a5d3de6b` (2026-09-07) | PHP (+JS) | 180 / ~14 K | 60 bad, 20 good | file | `vulnerabilities/<v>/source/{low,medium,high}.{php,js}` = bad, `impossible.php` = good, 19 vuln classes (sqli, xss_*, exec, fi, upload, csrf, ...). Best labeled good/bad contrast of the web apps. |
| `pygoat/` | github.com/adeyosemanputra/pygoat @ `19d17cc88748` (2026-03-28) | Python/Django | 202 / ~12 K | 34 bad | function (18 with `sink_line`) | Hand-curated lab views in `introduction/views.py` (SQLi raw(), pickle, yaml.load, XXE, subprocess shell, eval, SSRF, SSTI, md5, cookie trust ...). |
| `Vulnerable-Flask-App/` | github.com/we45/Vulnerable-Flask-App @ `b6a4f97afd46` (2021-05-11) | Python/Flask | 9 / 740 | 13 bad | line | Hand-curated from `app/app.py` (SQLi, SSTI, yaml.load, jwt verify=False, IDOR, hard-coded secrets, md5). |

- Caveats: **positives only are labeled** (except DVWA/juice-shop codefixes); unlabeled code is *not* known-clean, so precision on these is only a lower bound — use them for recall and for triaging FPs by hand. Hand-curated rows (NodeGoat, pygoat, Flask) are my reading of the code/comments, not an official answer key. Juice-shop codefixes are snippets, not compilable modules.

## 5. `rustsec/` — RustSec pairs from the local cargo cache (as requested)
- Source: https://github.com/rustsec/advisory-db @ `e2111519ba6d14a5da59a7b2e5c8083ae8a37c01` (2026-09-25); crates from `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/` (read only, copied — Cargo.toml, build.rs, `src/` only; never symlinked).
- Selection: 1,251 advisories; excluded 261 `informational = unmaintained|notice` and 25 withdrawn (kept `informational = unsound`); 698 name no crate present locally. Among the rest: **32 pairs** (a vulnerable AND a patched version both local; pair = the smallest local patched version and the largest local vulnerable version below it), **9 singles** (`singles.json`: vulnerable version only, with registry paths — not copied), 226 patched-only. `candidates.json` has every advisory's local version status.
- Layout: `RUSTSEC-*/{vuln,fixed}/`, `meta.json` (advisory fields, versions, `affected_functions`, per-hunk diff with enclosing fn on both sides, `fix_commit_overlap`, `diff_tightness`), `advisory.md`. `advisories.jsonl` = one row per advisory (versions, dirs, categories, tightness, `localization`, `vuln_functions`).
- ~3,068 files / ~1.17 M lines; 48 MB. Diff tightness: 6 tight, 17 medium, 9 loose (local versions are often far apart, e.g. ring 0.16.20 -> 0.17.14, idna 0.3.0 -> 1.0.3).
- Labels in `ground_truth.jsonl` (line rows per diff hunk + function rows per enclosing fn), with `relevance`:
  - `fix_commit` — the hunk's +/- lines appear in the advisory's actual fix commit/PR diff (fetched via GitHub API; `rustsec-adjacent/fix_diffs/`),
  - `name_match` — the hunk touches a name from `affected.functions` or an inline-code identifier in the advisory text,
  - `test` — hunk in `mod tests`/`tests/`, `diff` — anything else.
  - `label`: `bad`/`good` only for fix_commit/name_match rows (528 bad / 598 good); `bad_candidate`/`good_candidate` for unlocalized hunks of tight diffs (33/34); `context_vuln`/`context_fixed` for the remaining medium-diff hunks; `test_change`; plain hunks of loose diffs are dropped (counts remain in meta.json).
  - Localization per advisory: 16 fix_commit, 8 name_match, 1 tight_diff, 7 none.

## 6. `rustsec-adjacent/` — RustSec pairs at the exact fix boundary (extension, recommended)
- Same advisory set, but the pair is the **adjacent published versions** straddling the fix: the smallest non-yanked, non-prerelease patched version whose immediate predecessor is vulnerable. Taken from the local cache when present (read only; 45 of the 1,304 crate copies), otherwise downloaded from `static.crates.io` (src only). Library src > 40 MiB skipped (windows).
- **652 pairs** (121 `unsound`); 313 advisories skipped (`pairs_skipped.json`: 220 have no patched version, 91 have no vulnerable->patched adjacent published pair, 1 index 404, 1 too large). 46 pairs have no `.rs` change under `src/` (e.g. fix is a dependency bump) — listed in `advisories.jsonl` with `localization: no_rs_src_diff`.
- Diff tightness: **352 tight** (<= 200 changed lines, <= 5 files), 236 medium, 64 loose. Localization: **183 fix_commit, 40 rudra, 164 name_match, 130 tight_diff**, 89 none, 46 no_rs_src_diff.
- ~55.7 K files / ~26 M lines, 760 MB on disk (identical files between vuln/fixed and across advisories are **hardlinked** — do not edit in place).
- `ground_truth.jsonl`: 8,854 bad / 9,224 good (localized), 1,193/1,228 candidates, 18,651/19,119 context, 17,894 test_change. Also 84 `relevance: rudra_report` bad rows (Rudra report locations that land on this pair's vuln version).
- `fix_diffs/`: 198 advisories' fix commit / PR diffs (`<id>-<n>.diff`). `rudra_reported.json` is empty (advisory-db never mentions Rudra; the Rudra mapping is in `rudra/`).

## 7. `rudra/` — Rudra-reported Rust soundness bugs (cheap add-on)
- Source: https://github.com/sslab-gatech/Rudra-PoC (shallow, 2026-09-27): each `poc/*.rs` has TOML metadata `target.crate/version`, `rustsec_id`, `bugs[].bug_class`, `rudra_report_locations`.
- 146 crate versions (src only, from crates.io or the local cache) in `<crate>-<version>/`; 2,158 files / ~767 K lines; 31 MB.
- **261 bad line-level rows** (Rudra report spans, with enclosing fn): SendSyncVariance 140, UninitExposure 58, PanicSafety 47, HigherOrderInvariant 9, Other 7. 32 PoCs had no locations (manual finds). 8 locations point to files outside `src/` that were not copied.
- Caveats: positives only; the spans are what Rudra flagged (often the `unsafe impl Send/Sync` or the generic fn), confirmed as bugs by the maintainers/RustSec. Pair with `rustsec-adjacent` (same RUSTSEC id) for a fixed-version negative.

---

## Which corpus for what
- **C/C++ pattern rules**: `juliet-c` (function + flaw-line GT, 118 CWEs, balanced good/bad twins). Watch the synthetic regularity.
- **Java/JS/PHP/Python taint-ish rules**: `owasp-benchmark-java` (best: 2,740 balanced, CWE-labeled files built to punish FPs) and `juliet-java`; then `DVWA` (good/bad per level), `juice-shop` markers + codefixes; NodeGoat/pygoat/Flask for recall only.
- **Rust logic/soundness bugs**: `rustsec-adjacent` filtered to `diff_tightness == tight` and `localization in (fix_commit, rudra, name_match)` — fire near the fixed lines in `vuln/`, stay silent at the same fn in `fixed/`. `rudra/` for Send/Sync-variance / panic-safety / uninit classes. `rustsec/` (local-cache pairs) is small and its version gaps are wide; use it mainly because those versions are what this machine's dependency shards contain.

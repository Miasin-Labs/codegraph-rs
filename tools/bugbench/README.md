# bugbench

Scores `codegraph analyze bugs` and `analyze rules` against corpora with
known answers: NIST Juliet (C/C++, Java), OWASP Benchmark (Java),
deliberately vulnerable web apps, and RustSec vulnerable→fixed crate pairs.

The corpora are not in the repo (~1.9 GB). `corpora/` holds the scripts that
build them and `corpora/MANIFEST.md` says where each comes from, its label
granularity and its caveats. Point `BUGBENCH_ROOT` at the built directory.

```sh
cargo build --release --bin codegraph
export BUGBENCH_ROOT=/path/to/bench
python3 tools/bugbench/run.py rustsec-adjacent --jobs 8 \
    --cmd 'rules=analyze rules --builtin --json --tests'
python3 tools/bugbench/score.py rustsec-adjacent
```

`rudra` (Rudra's reported soundness bugs, positives only) scores like the
labeled corpora: a finding on a reported span is a TP, anything else
unlabeled. Juliet samples `--sample N` testcases per CWE (40) of a default
CWE list; `--cwe CWE690 --cwe CWE401` samples those CWEs instead, to score
a rule on its own CWE.

Every run indexes a copy of each unit under a scratch `CODEGRAPH_HOME` with
the atlas, dependency graphs and background work turned off, so nothing
touches `~/.codegraph` or the corpora.

Scoring: a finding inside a labeled bad function is a true positive, inside
a labeled good one a false positive, elsewhere unlabeled (reported apart).
For RustSec pairs it is differential: firing near the fix in `vuln/` and not
at the same function in `fixed/` is a catch; firing in both is "not
discriminating". Compare precision with the corpus base rate (bad ÷ labeled),
which is what firing at random would score.

## CodeQL and combining two engines

`codegraph analyze codeql --json` is a command like any other (it needs the
CodeQL CLI; run it only on the public corpora — see its license in
`codegraph analyze codeql --help`). `combine.py` compares two labels alone,
as a union and as an intersection (findings the other engine corroborates:
same file and function, a shared CWE), with bugbench's finding scoring and
the OWASP scorecard's per-test-case TPR/FPR (any finding, or one of the
test's CWE). Rule CWEs come from the built-in rules' tags and the SARIF
CodeQL left in the kept work dir, so run with `--keep`:

```sh
python3 tools/bugbench/run.py owasp-benchmark-java --jobs 1 --keep --timeout 7200 \
    --cmd 'rules=analyze rules --builtin --json --tests' \
    --cmd 'codeql=analyze codeql --json --tests --wait 7200' \
    --cmd 'combined=analyze codeql --builtin --json --tests --wait 7200'
python3 tools/bugbench/combine.py owasp-benchmark-java --a rules --b codeql --merged combined
```

`corpora/owasp_gt.py <BenchmarkJava checkout>` writes OWASP's
`ground_truth.jsonl` from `expectedresults-1.2.csv`.

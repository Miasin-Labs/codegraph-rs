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
    --cmd 'rules=analyze rules --builtin --json --tests --top 100000'
python3 tools/bugbench/score.py rustsec-adjacent
```

Every run indexes a copy of each unit under a scratch `CODEGRAPH_HOME` with
the atlas, dependency graphs and background work turned off, so nothing
touches `~/.codegraph` or the corpora.

Scoring: a finding inside a labeled bad function is a true positive, inside
a labeled good one a false positive, elsewhere unlabeled (reported apart).
For RustSec pairs it is differential: firing near the fix in `vuln/` and not
at the same function in `fixed/` is a catch; firing in both is "not
discriminating". Compare precision with the corpus base rate (bad ÷ labeled),
which is what firing at random would score.

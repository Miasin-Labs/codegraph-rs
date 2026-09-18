# What our agent sessions say codegraph should do

Research, 2026-09-17. Goal: tooling that genuinely saves coding agents tokens
and time, grounded in what agents on this machine actually did — not in what we
assume they need.

- **Diagrams:** [FLOWCHART.md](FLOWCHART.md) (the recurring patterns, rendered
  inline), [FLOWCHART.mmd](FLOWCHART.mmd) (the core loop),
  [TARGET.mmd](TARGET.mmd) (request → minimal answer → codegraph status), and
  every per-goal/per-study chart under [`flowcharts/`](flowcharts/).
- **Data** (not in this repo): mined from opencode, JFC, Claude Code and
  prime-agent session stores — 627k tool calls in the first pass, 768k with
  JFC — plus a 111k-vector MiniLM corpus of prompts, replies and labelled
  corrections. The corpus, embeddings, per-study reports, numbers
  (`*.json`) and reproduction scripts live outside the repo in the local
  session-mining store. They contain secrets from the raw sessions, so they
  stay local, owner-only; everything in this directory is aggregate.

## The studies

| study | question | sample |
|---|---|---|
| taxonomy (first pass) | what do agents call, and how often through the graph? | 627,116 calls, 10,399 sessions |
| **workflows** | what goals recur, and what process does each run? | 48,297 episodes (prompt → calls until the next prompt) |
| **consumption** | how much of each tool's output is ever used; what is the minimal sufficient answer? | 228,742 calls with outputs, 2,420 sessions |
| **replay** | can codegraph answer the same episodes, cheaper, with the same files? | 168 episodes, 15 projects, 12 models |
| **timeline** | how does behaviour change over time; how much is re-discovered across sessions? | 767,820 calls, 2026-02-14 → 09-18 |
| **failures** | where do agents go wrong, and could structure have prevented it? | 240 hand-labelled corrections from a 40k flagged corpus; build/edit error signals from 630k tool results |

## Findings

1. **Agents spend half their calls navigating, almost none of it through the
   graph.** 55.8% of calls are read/search/find. True graph usage is ~0.7% of
   calls (the first pass overcounted ~2× by matching any command that
   mentioned `codegraph`). Where codegraph *was* used (June, opencode/JFC:
   47–59% of sessions), sessions grepped far less (2.0 vs 6.5 greps per edit —
   suggestive, not causal).
2. **One loop dominates every goal:** read→read (112k transitions: paging,
   re-reading), search→search (59k: reformulating, one name per call),
   search→read (39k: opening every hit), then edit→build→edit. Understanding
   code (21.8%), fixing bugs (15.3%), surveying (11.4%) and locating
   definitions (11.4%) are the top goals; fixing a failing test is the most
   expensive (median 38 calls, ~29k output tokens, 6.6 min; p90 115 calls).
3. **63% of what tools return is never used.** Of 127M tokens of navigation
   and build output, a minimal answer would have been 47M. Agents use 12% of
   the lines they read and 39% of search hits; 8.5% of read bytes are re-sent
   unchanged; build/test output is 16–20% signal, and agents already pipe 38%
   of runs through `tail`/`grep` — then re-run when the filter hid the error.
4. **Codegraph's own output was part of the waste.** `explore` overflowed
   opencode's size cap in 42% of calls (≈18.5k tokens dropped each), and 30%
   of explores were followed by a grep for the same symbol. A `search` row is
   ~94 tokens where ~35 suffice. The replay found why: the MCP projection sent
   the full structured JSON as the text (and again as `structuredContent`), so
   depth limits and truncation never reached the client — `files{maxDepth:2}`
   on linux returned 5.8 MB — and the re-read ledger was wired only for
   `explore`, not `node`.
5. **Codegraph is much cheaper but finds fewer of the files that mattered.**
   A 2–3 call codegraph path used 92% fewer calls and 66% fewer bytes than the
   agents, at recall 0.56 vs 0.81; it was as good *and* cheaper in 56/168
   episodes. The ceiling is the single-call file budget (perfect terms: 0.49),
   and 60% of missed files sit one graph hop from a surfaced file (median
   fan-out 75 — a ranking problem). 27% of greps are literal/regex text, and
   42% of navigation leaves the index root (un-indexed dirs, `/tmp` clones,
   306 of 344 indexes on an older schema).
6. **Every session starts from zero.** 45% of the files a session touches were
   explored by an earlier session in the same repo; 59.6% of read bytes go to
   them and 80.6% of those were unchanged. 46% of searches repeat an earlier
   session's identifier. A session memory would save an estimated 57M tokens
   (range 11–191M) of ~723M. `codegraph history` — the intended flywheel — was
   never populated here, over-counts ~4.6×, records most shell commands as
   `cd`, is not idempotent, and its redaction misses common secret shapes.
7. **Most agent failures are not about code structure — but the structural
   ones are the ones a graph can stop.** Of 240 labelled corrections, 35% are
   genuine failures (95% CI 29–41%); 36% of those are structural (misread
   flow, an assumed in-repo or dependency API, a stale view of a file, a
   missed usage), and 33% were preventable with high or medium confidence —
   by `explore`, `node`, `callers`, `search`, `diagnostics`, or by capabilities
   codegraph lacks: a dependency-API index, a stale-edit guard, a dataflow
   trace. The rest are intent, external facts, runtime evidence and
   environment. The human caught 43% of genuine failures; the compiler 12%.
   Build logs show the same class at scale: 1,792 builds failed on a symbol
   that doesn't exist (E0425/E0599/E0432/E0433/TS2339) and 528 edits failed
   because the file had changed underneath.
8. **The harness mattered more than any codegraph release.** Graph usage
   tracked which agent app was in use (opencode/JFC with codegraph wired vs
   prime-agent without it, Claude Code grepping through Bash), not feature
   landings.

## What to build, ranked by evidence

| # | change | evidence | status |
|---|---|---|---|
| 1 | Bounded, compact wire output: honour limits, one payload not two, slim rows, a budget below the host cap | 5.8 MB `files` calls; 42% explore overflow; 94 → 35 tok rows | shipped: 24K default budget, `files` honours depth and pages, slim v2 rows (linux `files{maxDepth:1}` 5.76 MB → 942 B) |
| 2 | Re-read ledger on every source-emitting tool (`node`, not just `explore`) | 8.6M tok re-sent; 97% saving per re-read; 4–8 re-reads per episode | shipped: `node` records and honours the ledger (a repeat file read 13.5 KB → 424 B) |
| 3 | Session memory: fixed `history` ingest + redaction, then `recall` and a ≤1 KB session digest | 45% cross-session re-discovery; ~57M tok | ingest fixed (one row per call, idempotent, 3.8× over-count gone, redaction of sudo passwords/keys/JWTs/emails); `recall` + session digest designed, not built |
| 4 | Explore that ranks and folds in 1-hop neighbours (edges, co-change, tests) under a hard budget | 60% of replay misses; 0.49 single-call ceiling | gap |
| 5 | Symbol windows instead of files: `node(file, line)` → enclosing span; outline mode; cursor paging | 48.5M tok of read lines unused; 12.2M in paging chains | partial |
| 6 | Text search over indexed files returning hits + enclosing symbol, per-file caps | 27% of greps are text; >100-hit searches = half of search tokens | gap |
| 7 | Build/test output as errors-on-symbols | 76–80% noise; 886 pure re-runs | shipped (`diagnostics`) for check/clippy/tsc; tests pending |
| 8 | Reach: upgrade old-schema indexes in the background; index reference clones on demand | 4,165 sessions unreplayable; 42% of navigation outside the root | gap |
| 9 | Precision of edges agents rely on (callers of non-callables → xref; enum type refs; receiver-typed Rust methods) | `callers` found 7/36 referencing files | receiver-typed Rust methods shipped (1,452 guessed std-method edges → 0; name-similarity `Type::m` 1,459 → 0; partial qualified matches need a name boundary); non-callables → xref and enum type refs still a gap |
| 10 | Dependency-API lookup (the pinned version's signatures) and a stale-edit guard (fingerprint check before an edit is built on a read) | 5 + 4 of 83 labelled failures; 1,792 unknown-symbol builds; 528 stale edits | gap |
| 11 | Big-repo latency: stream or return partial results within a deadline | explore 41–92 s on the two largest indexes (3.9–4.5 GB); 46 historical timeouts | gap |

## Caveats

Consumption means *observable* reuse (an agent can understand without echoing),
so "never used" is an upper bound. Replay recall is file-level against edited or
cited files, and cited ground truth flatters the agent. Harness, model and
codegraph changes overlap in time, so no before/after comparison here is causal.
Token counts are bytes/4.

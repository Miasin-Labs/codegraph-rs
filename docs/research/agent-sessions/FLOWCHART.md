# The patterns agents repeat, and what codegraph should answer instead

Diagrams distilled from 627k–768k mined agent tool calls (opencode, JFC,
Claude Code, prime-agent; 2026-02 → 2026-09). Numbers come from the studies
summarised in [README.md](README.md); every per-goal and per-study chart is in
[`flowcharts/`](flowcharts/) as a standalone `.mmd` file.

## 1. The loop every goal runs

Whatever the prompt asks for, agents run the same navigation loop before they
edit. The three self-loops are where calls and tokens go.

```mermaid
flowchart TD
  P([user prompt]) --> F["find files<br/>glob / ls / find"]
  P --> S
  P --> R
  F --> S["search text<br/>grep / rg"]
  S -->|"59,300× search→search<br/>reformulate; one name per call;<br/>82% of greps are regexes"| S
  S -->|"38,888× search→read<br/>open every hit"| R["read file"]
  R -->|"112,019× read→read<br/>paging, re-reading"| R
  R --> S
  R --> E["edit"]
  E --> B["build / test"]
  B -->|"edit→build→edit<br/>fix_test: median 38 calls, 6.6 min"| E
  B --> D([answer / done])
  R --> D

  classDef hot fill:#fbe3e1,stroke:#b3261e,color:#111
  class S,R hot
```

| what the loop costs | measured |
|---|---|
| lines of a read the agent ever uses | **12%** (whole-file reads: 37% of bytes near anything used) |
| search hits the agent ever uses | **39%** (10% strictly); >100-hit searches are half of all search tokens at 28% use |
| read bytes re-sent unchanged in the same session | **8.5%** (median 4 calls after the first send) |
| build/test output that is signal | **16–20%** (passing runs: 11%) |
| calls that are verbatim repeats in the same session | 28.7% overall, falling to 2.6–7% by 2026-09 with newer models |

## 2. How goals chain inside a session

Navigation goals feed the fix loop; `fix_bug` re-enters itself 42% of the time.
(Top transitions only; full chart: `flowcharts/workflows/FLOWCHART_OVERVIEW.mmd`.)

```mermaid
flowchart LR
  U([prompt]) -->|1778| UC["understand how X works<br/>21.8% of episodes"]
  U -->|1539| LD["locate a definition<br/>11.4%"]
  U -->|1279| SR["survey the repo<br/>11.4%"]
  U -->|1175| FB["fix a bug<br/>15.3%"]
  LD -->|"29%"| UC
  SR -->|"30%"| UC
  UC -->|"repeat 47%"| UC
  UC -->|"18%"| FB
  LD -->|"18%"| FB
  FB -->|"repeat 42%"| FB
  FB -->|"23%"| UC
  FB --> VCS["commit / vcs ops"]
  FT["fix a failing test<br/>3.6% · most expensive"] -->|"25%"| UC
```

## 3. Where the tokens go

Of 127M tokens of navigation and build output (228k calls with outputs),
**37% was sufficient**; the rest was never observably used.

```mermaid
flowchart LR
  NAV["navigation + build output<br/>127.1M tok"] --> MIN["minimal sufficient answer<br/>46.7M tok · 37%"]
  NAV --> W["never used afterwards<br/>80.4M tok · 63%"]
  W --> W1["read lines outside any used window<br/>48.5M"]
  W --> W2["search hits never used<br/>15.0M"]
  W --> W3["content re-sent unchanged<br/>8.6M"]
  W --> W4["passing-test / progress lines<br/>5.3M"]
  classDef used fill:#e3f4e1,stroke:#2f7d32,color:#111
  classDef waste fill:#fbe3e1,stroke:#b3261e,color:#111
  class MIN used
  class W,W1,W2,W3,W4 waste
```

## 4. The same episodes, answered by codegraph

168 real episodes replayed against today's indexes. A rule-built 2–3 call
codegraph path is far cheaper but finds fewer of the files that mattered.

```mermaid
flowchart LR
  E(["real episode<br/>median 24 nav calls · 77 KB<br/>agent saw 0.81 of the files that mattered"])
  E --> B1["explore(prompt terms)<br/>~18 KB · recall 0.34"]
  B1 --> B2["search(symbols batch)<br/>cumulative recall 0.48–0.56"]
  B2 --> B3["goal step: callers / node outline / files<br/>median 37 KB total · 2–3 calls"]
  B3 --> OUT{{"92% fewer calls · 66% fewer bytes<br/>recall 0.56 · as good and cheaper in 56/168"}}
  OUT --> M1["60% of misses: one graph hop from a surfaced file<br/>(median fan-out 75 → needs ranking)"]
  OUT --> M2["27% of greps are literal/regex text, not symbols"]
  OUT --> M3["42% of navigation leaves the index root"]
  OUT --> M4["wire bugs: files ignores maxDepth (5.8 MB),<br/>node re-reads not de-duplicated"]
  classDef gap fill:#fff6d9,stroke:#9a7b12,color:#111
  class M1,M2,M3,M4 gap
```

Per goal (median): symbol lookup 20 calls/70 KB → 2/37 KB; how-it-works 24/88 →
2/38; change impact 24/74 → 3/38; grep alternation 23/68 → 2/38; file paging
30/80 → 3/39; structure survey 26/145 → 2/23. One explore with *perfect* terms
still reaches only 0.49 recall — the single-call file budget, not query
wording, is the ceiling.

## 5. Across sessions, agents start from zero

In git repos, **45%** of the files a session touches were already explored by
an earlier session in the same repo; **59.6%** of read bytes go to such files
and **80.6%** of those files had not changed since. 46% of searches repeat an
identifier an earlier session already searched. 89% of repeats fall within 30
days.

```mermaid
flowchart LR
  S1["session 1<br/>grep → read A, B, C → edit B → test"] --> L(["session ends:<br/>nothing persisted"])
  L -->|"34% < 1 day · 89% < 30 days"| S2["session 2, same repo<br/>grep the same identifiers<br/>re-read A, B, C (80.6% unchanged)"]
  S2 -.->|"with a session-history graph"| RC["recall(path | symbol)<br/>~1–2 KB: last episodes, X at B:120,<br/>test Y failed then passed, A and C unchanged"]
  RC --> T["targeted read of B only"]
  classDef waste fill:#fbe3e1,stroke:#b3261e,color:#111
  classDef save fill:#e3f4e1,stroke:#2f7d32,color:#111
  class S2 waste
  class RC,T save
```

Estimated saving from a "we've been here before" memory: 11 / **57** / 191 M
tokens (low / mid / high) of ~723 M tokens of tool output.

## 6. Target: each recurring request → its minimal answer

What each request kind should return, and where codegraph stands.
Green = shipped on `feat/compiler-diagnostics`, red = gap.

```mermaid
flowchart LR
  A([agent request]) --> K1["where is X, Y, Z"]
  A --> K2["how does X work"]
  A --> K3["what is at file:line /<br/>read before edit"]
  A --> K4["read it again"]
  A --> K5["who uses X / blast radius"]
  A --> K6["is it broken"]
  A --> K7["which tests cover X"]
  A --> K8["what changes with X"]
  A --> K9["literal text / regex"]
  A --> K10["what did we do last time"]

  K1 --> T1["search symbols[] → rows: name kind file:line signature<br/>594 → 252 tok"]
  K2 --> T2["explore: rows + edges, code for top 3,<br/>ranked 1-hop files, budget under host cap<br/>4,267 → 1,264 tok"]
  K3 --> T3["node(file, line) → enclosing symbol span<br/>1,607 → 806 tok"]
  K4 --> T4["alreadySent reference or diff<br/>1,301 → 41 tok"]
  K5 --> T5["callers / impact; xref fallback for non-callables"]
  K6 --> T6["diagnostics: errors on symbols, counts for the rest<br/>528 → 126 tok"]
  K7 --> T7["tests(symbol)"]
  K8 --> T8["history(symbol) — git co-change"]
  K9 --> T9["text search over indexed files,<br/>hits + enclosing symbol, per-file caps"]
  K10 --> T10["recall(path | symbol | failures) + 1 KB session digest"]

  classDef shipped fill:#e3f4e1,stroke:#2f7d32,color:#111
  classDef flight fill:#fff6d9,stroke:#9a7b12,color:#111
  classDef gap fill:#fbe3e1,stroke:#b3261e,color:#111
  class T1,T3,T4,T6,T7,T8,T9,T10 shipped
  class T2,T5 gap
```

## 7. Where agents go wrong

240 labelled corrections, sampled from 40,000 the corpus flags as a correction,
reversal, recheck or "that's wrong". Only a third are genuine agent failures,
and only a third of *those* are structural — the part a code graph can reach.
The build logs agree: of 38,303 build results, 1,792 failed on a symbol that
does not exist (top codes E0425, E0599, E0432, E0433, TS2339 — names, methods,
imports and paths the agent assumed), and 528 of 45,668 edits failed because
the file had changed under the agent.

```mermaid
flowchart LR
  C["240 labelled corrections<br/>(stratified sample of 40,000 flagged)"] --> NF["not an agent failure · 65%<br/>new request 63 · routine check 33<br/>deliberation 30 · pasted content 19 · other 12"]
  C --> G["genuine failure · 35% (83)<br/>95% CI 29–41%"]
  G --> ST["structural · 36% (30)<br/>misread control/data flow 9<br/>assumed dependency API 5 · stale view of a file 5<br/>assumed in-repo API 4 · missed a usage 3<br/>repo layout 2 · wrong target 1 · config keys 1"]
  G --> NS["not structural · 64% (53)<br/>misread intent 11 · wrong external fact 11<br/>misread runtime evidence 9 · environment 8<br/>edit mechanics 5 · design rejected 5 · domain logic 3"]
  G --> DET["caught by<br/>the human 36 · runtime 11 · self-review 11<br/>tool error 10 · compiler 10 · tests 4"]
  ST --> P["preventable, high/medium confidence<br/>33% of genuine failures"]
  NS -. rarely .-> P
  P --> HAVE["existing codegraph<br/>explore 12 · node 7 · callers 6<br/>search 5 · diagnostics 3 · xref 2"]
  P --> MISS["missing capability<br/>dependency API index 5 · stale-edit guard 4<br/>dataflow trace 3 · verify gate 3 · sibling paths 2"]
  classDef waste fill:#fbe3e1,stroke:#b3261e,color:#111
  classDef save fill:#e3f4e1,stroke:#2f7d32,color:#111
  classDef gap fill:#fff6d9,stroke:#9a7b12,color:#111
  class G,ST,NS waste
  class P,HAVE save
  class MISS gap
```

## Per-goal and per-study charts

| study | charts |
|---|---|
| workflows (per-goal state machines) | `flowcharts/workflows/FLOWCHART_<goal>.mmd` — understand_code, fix_bug, survey_repo, locate_def, debug_runtime, vcs_ops, implement_feature, fix_compile, fix_test, refactor, impact_analysis, trace_flow, plus `FLOWCHART_OVERVIEW.mmd` |
| consumption (tool output → used → wasted) | `flowcharts/consumption/FLOWCHART_{OVERVIEW,SEARCH,READ,CODEGRAPH,BUILD,MINIMAL}.mmd` |
| replay (agent path vs codegraph path) | `flowcharts/replay/FLOWCHART_{SYMBOL_LOOKUP,HOW_WORKS,CHANGE_IMPACT,GREP_ALTERNATION,FILE_PAGING,STRUCTURE_SURVEY}.mmd` |
| failures (root causes → preventing capability) | `flowcharts/failures/FLOWCHART_FAILURES.mmd` |
| timeline (over time, across sessions, recall design) | `flowcharts/timeline/FLOWCHART_{TIMELINE,REDISCOVERY,SCHEMA,INGEST_QUERY}.mmd` |

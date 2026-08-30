# CodeGraph Parity Gap: Rust port (1.3.1) → TS reference (1.6.0)

Read-only audit. Compares the Rust port at `/home/cole/RustProjects/active/codegraph-rs`
(v1.3.1 baseline) against the TS reference `/home/cole/WebstormProjects/forks/codegraph`
(v1.6.0), covering CHANGELOG entries for 1.3.2 → 1.6.0 plus `[Unreleased]`.

Method: extracted every CHANGELOG item > 1.3.1, grepped the Rust `src/` tree for the
matching symbol/concept/env var, and cross-checked `git log v1.3.1..HEAD` (234 commits).
Each item is marked PRESENT / PARTIAL / MISSING with file:line or "no hits" evidence.

Legend: **P0** = correctness/hang/crash or data-loss risk; **P1** = notable accuracy or UX
gap; **P2** = minor/cosmetic or niche.

Note on 1.5.0: the "native Rust kernel" release is largely N/A for the Rust port — the
Rust port *is* a native engine, so `CODEGRAPH_KERNEL`, wasm fallback, and byte-for-byte
kernel verification do not apply. What *does* apply from 1.5.0 is the resolution/indexing
performance and adaptivity work, tracked below.

---

## 1. Indexing reliability, hang-safety, and disk usage

- [MISSING] **Indexing-liveness / safety watchdog (disk-progress aware)** — the only watchdog
  in Rust is a PPID (parent-death) watchdog (`src/mcp/proxy/watchdog.rs`, `src/mcp/daemon.rs`).
  There is no equivalent of the TS "stuck-process watchdog" that kills a hung index but spares a
  healthy-but-slow one by checking DB file progress (1.3.1 #1212, 1.4.1 #1231, 1.6.0 #1431).
  Evidence: `grep -rni 'liveness|disk.*progress|making progress'` → no hits in an indexing path.
  Impact: **P1** — a Rust port that never self-kills a hung index also never protects against
  a real hang; less urgent than in Node (no single event loop) but the "still-progressing"
  heuristic and its `status` surfacing are absent.

- [MISSING] **WAL heal / bounded WAL resting size** (1.6.0 #1431) — `CODEGRAPH_WAL_HEAL_MB`
  has no hits; there is no fold-back-and-trim of an oversized leftover WAL on project open.
  Rust runs `PRAGMA wal_checkpoint(PASSIVE)` in `run_maintenance` (`src/db/connection.rs:337`)
  but nothing bounds a WAL left behind by a force-killed session. Evidence: `CODEGRAPH_WAL_HEAL_MB`,
  `wal_heal`, `heal` → no hits. Impact: **P0** — reported as tens of GB of disk leak on
  Windows; a force-killed Rust session has the same failure mode.

- [MISSING] **WAL-defer / checkpoint valve on slow storage** (1.4.0 #1231) —
  `CODEGRAPH_NO_WAL_DEFER` / `CODEGRAPH_WAL_VALVE_MB` have no hits. Rust checkpoints passively
  in `run_maintenance` but does not defer/coalesce checkpoints across a bulk index or sync.
  Impact: **P1** — on mechanical HDD / network disk this is the difference between <1 min and
  25+ min for a mid-size project (TS benchmark). Rust's write path is native so the constant
  differs, but the coalescing strategy is absent.

- [MISSING] **`CODEGRAPH_PARSE_TIMEOUT_MS` / bounded large-file parse budget** (1.4.0 #1231,
  1.6.0 #1555) — no hits for `CODEGRAPH_PARSE_TIMEOUT_MS`, `parse_timeout`, or a per-file parse
  deadline. Rust guards only by max **size** (`size_exceeded` warning, `src/extraction/orchestrator/parse.rs:420`),
  not by a wall-clock parse timeout. A data-only C/C++ header near the size limit that parses
  slowly cannot be time-bounded. Impact: **P1**.

- [PARTIAL] **Deep-nesting crash safety** (1.6.0 #1581) — Rust *does* guard recursion via
  `ensure_sufficient_stack(...)` in several walkers (`rust.rs:53`, `cobol.rs:424`,
  `tree_sitter_wrapper/{body_traversal,object_literals,pascal_calls,type_annotations}.rs`).
  But there is no evidence of the full "hand the file to a fallback parser and record a parse
  warning, then continue" recovery that TS added for pathological C/C++ nesting. Verify the
  guard covers the C/C++ native walker specifically. Impact: **P1** — a segfault on one file
  should not kill the whole index.

- [MISSING] **0-symbol self-heal after interrupted parse** (1.5.0 regression fix #1541) — no
  hits for a "file recorded with 0 symbols is detected and repaired on next sync" path
  (`heal`, `symbol_free`, `zero.symbol` → no hits in parse/sync). A crashed/timed-out first
  parse whose retry stores an empty result would leave symbols permanently missing until edit.
  Impact: **P1** (silent data loss).

- [MISSING] **Record skip reasons for too-large / repeatedly-failing files** (1.6.0 #1557) —
  files skipped for size get a warning but there is no evidence they are *recorded with the
  reason* so they are not re-discovered and retried every status/sync. Impact: **P2**
  (wasted work each sync, not incorrect).

- [PARTIAL] **Comment-stripped salvage flagged as a warning** (1.6.0 #1565) — Rust records
  `size_exceeded` warnings, but the comment-stripped last-resort fallback + visible warning
  (so a fresh index does not silently disagree with a re-parse) was not found. Impact: **P2**.

## 2. Resolution accuracy and graph correctness

- [PARTIAL] **Rust: generic/lifetime `impl` methods attributed to the type, not the trait**
  (1.6.0 #1588) — `get_receiver_type` in `src/extraction/languages/rust.rs:272` takes the
  **last** direct `type_identifier` child of an `impl_item`. For `impl<T> Trait for Type<T>`
  the implementing `Type<T>` is a `generic_type` node (not a direct `type_identifier`), so the
  last direct `type_identifier` is the **trait** — the exact bug #1588 fixed. The generic
  branch only handles `impl<T> MyStruct<T>` (inherent impls), not `impl<T> Trait for Type<T>`
  or `impl<'a> Trait for Type<'a>`. Impact: **P0** for Rust codebases — trait-impl methods
  mis-attributed, phantom edges, "who calls Type::method" fails.

- [MISSING] **Rust: `self.field.method()` resolved by the field's declared type** (1.6.0 #1585)
  — the receiver-inference dispatch (`src/resolution/name_matcher/receiver.rs`) exports only
  Go (`match_go_field_chain_call`), C++ (`infer_cpp_receiver_type`), and JVM
  (`infer_java_field_receiver_type`) field-type inference. No Rust field-chain resolver.
  Rust `self.inner.run()` falls back to bare-name matching → fabricated recursion / wrong edges.
  Impact: **P0** for Rust codebases (this is the port's own language).

- [MISSING] **TS: calls to exported object-literal namespace members** (1.6.0 #1573) —
  `export const api = { call() {} }; api.call()` resolving to the method. No object-literal /
  namespace-member resolver found in `src/resolution/name_matcher/`. Impact: **P1** — common
  TS API-surface pattern reports zero callers.

- [MISSING] **TS: import aliases via `tsconfig` `extends` / base configs / `baseUrl`**
  (1.6.0 #1534) — `src/resolution/import_resolver/aliases.rs` reads `compilerOptions.paths`
  but there are **no** hits for `extends`, `tsconfig.base`, or `baseUrl` chain-following.
  Nx-style monorepos keep aliases in `tsconfig.base.json`; without this, every cross-package
  import falls back to name-only matching. Impact: **P1** (monorepo impact/callers wrong).

- [MISSING] **Python: classes used as values produce reference edges** (1.6.0 #1478) —
  `return SomeSerializer`, `handler = SomeClass`, registry dicts. No class-as-value handling
  found in the Python extractor. Impact: **P1** (Django/DRF callers/impact miss views).

- [MISSING] **SAP HANA `.xsjs` / `.xsjslib` import resolution** (1.6.0 #556) — no hits for
  `xsjs`/`xsjslib` anywhere in Rust `src/`. Impact: **P2** (niche language).

- [PRESENT] **Forward + reverse unresolved-ref retry on sync/edit** (1.4.1 #1240) — implemented
  in `src/codegraph.rs:index_files_locked` (union of pre/post-edit changed node names, then
  `get_unresolved_references_by_names` re-resolve) and reverse case via
  `restore_unresolved_refs_for_removed_targets` (`src/extraction/orchestrator/reconcile.rs`).

- [MISSING] **Anti-drift rebind of same-named defs on aging index** (1.6.0, `CODEGRAPH_NO_REBIND`)
  — no hits for `CODEGRAPH_NO_REBIND` or a rebind pass. A long-lived synced index drifts from a
  fresh one (TS: 5.7% → 1.3% wrong edges). Impact: **P1** (answers silently degrade as index ages).

- [PRESENT] **Erlang extractor** — `src/extraction/languages/erlang.rs` exists and handles
  `-export`, specs, docstrings. **[PARTIAL]** the 1.6.0 #1610 change to give same-name/
  different-arity functions distinct `module:fun/arity` node identity is not clearly present:
  node names are created from the bare atom (`create_node(NodeKind::Function, &name, ...)`,
  erlang.rs:159) with no `/arity` suffix; `arity` count is low (3). Likely still collapses
  arities into one node → self-loops instead of `f/1 → f/2` edges. Impact: **P1** (Erlang).

- [PRESENT] **Unions as first-class nodes** (1.6.0 #1515) — C/C++ union handling present
  (`c_cpp.rs`, and `rust_union_keeps_fields_and_impl_relationships` test in `rust.rs:508`).

- [PARTIAL] **Batch-boundary under-count / "Maximum call stack" on large sync** (1.6.0 #1558) —
  the TS crash is a Node/V8 recursion limit that does not exist in Rust, but the underlying
  correctness bug (references dropped when a caller's repeated call sites split across a batch
  boundary; post-pass cleanup removing not-yet-attempted rows) should be verified in
  `src/resolution/resolver/batch.rs`. Evidence: no explicit per-row targeted-cleanup marker found.
  Impact: **P1** (deterministic missing edges) — confirm before closing.

## 3. Performance and adaptivity

- [PRESENT] **Parallel reference resolution** — `src/resolution/resolver/parallel.rs`
  (`resolve_all`, `resolve_all_parallel`, JoinSet, `worker_count_for`).
- [PARTIAL] **Adaptive engagement + env tunables** (1.5.0) — worker count derives from
  `std::thread::available_parallelism()` (`parallel.rs:11`) but is **not** cgroup/container-aware,
  not memory-budget-aware, and not adaptively measured per-project. `CODEGRAPH_NO_PARALLEL_RESOLVE`,
  `CODEGRAPH_PARALLEL_RESOLVE_MIN`, and `CODEGRAPH_RESOLVE_WORKERS` all have **no hits** — the TS
  overrides and the min-work threshold are missing. Impact: **P1** on constrained CI/VPS
  (risk of OOM) and no operator override.
- [MISSING] **Container/cgroup-aware core & honest available-memory sizing** (1.5.0) —
  `cgroup`/`available_mem` → no hits. Worker pools sized from host, not the container allowance.
  Impact: **P1** (OOM risk in Docker/CI).
- [MISSING] **Fast-init durability deferral / store-worker** (1.5.0) —
  `CODEGRAPH_NO_FAST_INIT`, `CODEGRAPH_NO_STORE_WORKER` → no hits. Impact: **P2** (speed only).
- [PRESENT-ish] **Name-index seek for exact lookups** (1.6.0 #1542) — Rust uses FTS5 + name
  segment vocab; confirm exact-name lookups go through the name index, not full scans. **P2**.

## 4. MCP / tooling

- [PRESENT] **All 8 TS MCP tools** — search, callers, callees, impact, node, explore, status,
  files are registered in `src/mcp/tools/` (plus Rust extras: analysis, arch, paths, vuln,
  verify_roles, xref).
- [PRESENT] **`codegraph_explore` cross-call dedup** (1.6.0) — `CODEGRAPH_EXPLORE_DEDUP` in
  `src/mcp/explore_session/mod.rs:18`; dedup session module present.
- [MISSING] **MCP: adopt a single indexed sub-project below the launch dir** (1.6.0 #1606) —
  `find_nearest_codegraph_root` (`src/directory.rs:76`) only walks **up**. It never picks the
  one indexed project **below** a workspace/monorepo container root, so a server launched a
  level too high fails every tool call. Impact: **P1** (common monorepo/agent-workspace setup).
- [MISSING] **MCP: clear "no project resolved" startup + nearby-project hints** (1.6.0 #1607) —
  no evidence the server logs the searched dir and lists indexable sub-projects when nothing
  resolves. Impact: **P2** (diagnosability).
- [MISSING] **Live scope refresh when `codegraph.json` / `.gitignore` changes at runtime**
  (1.6.0 #1590) — the watcher special-cases the config filename in
  `src/sync/watcher.rs:366` only to *exclude it from indexing*; there is no re-load of the
  ignore matcher / full reconcile when `exclude`/`include` change, so a running server keeps its
  startup scope (the exact bug #1590 fixed). Impact: **P1** (exclude appears not to work until restart).
- [MISSING] **MCP update-available check** (1.4.1 #1243) — `CODEGRAPH_NO_UPDATE_CHECK`,
  `update_check`, `latest.*release` → no hits. Impact: **P2**.
- [PRESENT] **MCP connect message advertises 30+ languages** (1.6.0 #671) — verify wording;
  the instructions surface exists. **P2**.

## 5. CLI / installer

- [PRESENT] **`codegraph context <task>` command** (1.6.0 #1611) — `Commands::Context` in
  `src/bin/codegraph/cli.rs` and `src/bin/codegraph/context.rs`.
- [PRESENT] **`deprioritize` config** (1.6.0 #982) — `src/search/query_utils.rs:412,760`,
  `src/context/builder.rs:1280`.
- [PRESENT] **GitHub Copilot install targets** (1.6.0) — `src/installer/targets/`:
  `copilot_cli`, `copilot_jetbrains`, `copilot_vscode` (53 hits).
- [MISSING] **`codegraph install --init` and `codegraph init --yes`** (1.6.0 #1578) — `Install`
  has `target/location/yes/no_permissions/print_config` but **no `--init`**; `Init` has only
  `path/force/verbose` — **no `--yes`**. One-shot non-interactive bootstrap
  (`install --yes --init`) is not possible. Impact: **P1** (CI / fresh-container onboarding).
- [PARTIAL] **Per-project Codex install `--location=local`** (1.6.0 #1531) — `Install` has a
  `location` arg ("global"/"local"); confirm the Codex target writes `./.codex/config.toml` +
  project `AGENTS.md` block. **P2**.
- [MISSING] **`codegraph install --refresh` / auto-refresh agent config on upgrade** (1.4.1 #1238)
  — no `--refresh` flag and no hits for `CODEGRAPH_NO_INSTALL_REFRESH`. Impact: **P2**.
- [MISSING] **`NO_COLOR` / `--color` / `--no-color`, auto-plain when piped** (1.5.0 #1281) —
  `NO_COLOR`, `FORCE_COLOR` → no hits (only `CODEGRAPH_ASCII`/`CODEGRAPH_UNICODE` exist).
  Impact: **P2** (piped output may carry ANSI/progress control chars).
- [PRESENT] **Telemetry: first-party endpoint** (1.6.0) — `telemetry.getcodegraph.com/v1/events`
  in `src/telemetry.rs:14`, with `CODEGRAPH_TELEMETRY` / `DO_NOT_TRACK` off-switches.

## 6. Ranking / explore quality (1.6.0 batch #1500, #1474, #1475)

Most of the 1.6.0 `codegraph_explore` ranking fixes were not individually verified in Rust and
are likely PARTIAL/MISSING — the Rust explore path exists but the specific fixes below need
targeted confirmation:

- [PRESENT] **Generated-file detection by banner** (1.6.0 #1500) — `has_generated_header`
  (`src/extraction/generated_detection.rs:114`) matches `Code generated … DO NOT EDIT.` /
  "Generated by … running …" style banners, not just filenames. Good.
- [MISSING?] **Stale-slice safety: verify file against index before showing sliced code**
  (1.6.0 #1474) — not confirmed; a file changed on disk after last sync could be served as a
  wrong slice via `codegraph_node`/`explore`. Impact: **P1** (serves wrong code as verbatim) —
  worth a focused check.
- [MISSING?] **Indirect test coverage via caller chains (3 hops)** in blast-radius (1.6.0 #1475).
  Impact: **P2**.
- [MISSING?] **Explore query matching: camelCase word-splitting, kebab-basename pinning,
  path-name pinning, variables/constants as seeds** (1.6.0 + 1.5.0 #1196). Needs verification.
  Impact: **P1** (answer relevance).

---

## Top 10 to port (ordered)

1. **[P0] Rust `self.field.method()` receiver inference** (#1585) — add a Rust field-chain
   resolver alongside Go/C++/JVM in `src/resolution/name_matcher/receiver/`. The port's own
   language currently fabricates recursion / wrong edges.
2. **[P0] Rust generic/lifetime `impl Trait for Type<T>` attribution** (#1588) — fix
   `get_receiver_type` in `rust.rs` so the implementing type (not the trait) wins for
   `generic_type`/lifetime impls.
3. **[P0] WAL heal / bounded resting WAL size** (#1431, `CODEGRAPH_WAL_HEAL_MB`) — fold back and
   trim an oversized leftover WAL on open; stops the tens-of-GB disk leak after force-kill.
4. **[P1] MCP scope refresh on `codegraph.json` / `.gitignore` change** (#1590) — reload ignore
   matcher + full reconcile at runtime so `exclude` works without a restart.
5. **[P1] MCP adopt single indexed sub-project below launch dir** (#1606) — make project
   resolution look downward for a lone indexed project, not just upward.
6. **[P1] Anti-drift rebind pass** (`CODEGRAPH_NO_REBIND`) — keep a long-lived synced index from
   diverging from a fresh index on same-named definitions.
7. **[P1] 0-symbol self-heal after interrupted parse** (#1541) — retries must store real symbols;
   detect & repair files recorded with 0 symbols on next sync.
8. **[P1] `install --init` + `init --yes`** (#1578) — one-shot non-interactive bootstrap for CI /
   fresh containers.
9. **[P1] Adaptive/container-aware worker sizing + resolve-worker env overrides** (#1231 / 1.5.0)
   — cgroup-aware cores, honest available memory, `CODEGRAPH_RESOLVE_WORKERS` /
   `CODEGRAPH_NO_PARALLEL_RESOLVE` / `CODEGRAPH_PARALLEL_RESOLVE_MIN`; avoids OOM on VPS/CI.
10. **[P1] TS object-literal namespace members (#1573) + tsconfig `extends`/base alias chains
    (#1534)** — restore cross-file callers/impact for the two most common TS/monorepo patterns.

---

### Verified PRESENT (no action)

MCP tool set (8/8), `codegraph_explore` dedup, `codegraph context` command, `deprioritize`,
Copilot install targets, unions as nodes, forward/reverse unresolved-ref retry (#1240),
parallel resolution (non-adaptive), generated-banner detection, first-party telemetry endpoint,
schema at v9 (parity with TS `CURRENT_SCHEMA_VERSION = 9`), PPID watchdog, `ensure_sufficient_stack`
recursion guards, embedded-repo / untracked-file discovery via git.

### Needs a focused follow-up check (not conclusively classified)

Stale-slice safety before showing code (#1474), explore query camelCase/kebab/path matching
(#1196 + 1.6.0), indirect test-coverage 3-hop (#1475), batch-boundary edge under-count (#1558),
deep-nesting fallback-parser recovery for the native C/C++ walker (#1581).

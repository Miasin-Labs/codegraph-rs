# TS ↔ Rust behavioral parity report

## 1.3.1 parity update (2026-07-09)

The detailed counts below are the preserved 0.9.9 row-level snapshot. Since
that run, the Rust implementation has incorporated the upstream 1.3.1 surfaces
that materially changed compatibility and coverage:

- schema v8 reconciles both incompatible schema-7 lineages, deduplicates
  existing edges, and enforces a unique logical edge identity;
- index metadata records extraction version, build state, partial/failure
  outcomes, and discovered/indexed/skipped/error file accounting;
- resolver snapshots use compact indexed storage and synthesized edges persist
  in bounded chunks with language-gated passes;
- receiver inference covers scoped locals, typed parameters, and chained
  receivers, with Metal/CUDA preprocessing;
- the current language, framework, and dynamic-dispatch matrices are wired,
  including ArkTS, Razor, Astro, R, Solidity, Nix, CFML dialects, COBOL,
  VB.NET, Erlang, and Terraform/OpenTofu;
- `codegraph.json`, `explore`, `node`, `daemon`, telemetry, upgrade, version
  aliases, prompt-hook, and Claude installer integration are present.

This update did not overwrite the old count tables with incomparable numbers.
A new two-arm row-level fixture run should be recorded as a separate snapshot.

Date: 2026-06-06.
Fixture: full repo copy (TS + Rust sources + SQL + MD, **353 indexed files**) at
`/tmp/cg-parity/fixture`, excluding `.git`, `node_modules`, `dist`, `rust/target`,
`.codegraph` (and stray `core.*` dumps). Each arm ran `init` (index included) on the
identical tree; aggregates dumped from `.codegraph/codegraph.db` with sqlite3.

- TS arm: `dist/bin/codegraph.js` (fresh `npm run build`, v0.9.9) — 9,105 nodes / 31,170 edges / 3.2s
- Rust arm: `rust/target/release/codegraph` (fresh `cargo build --release`) — 9,105 nodes / 31,112 edges / 1.7s

## Headline

**Nodes, node IDs, and files are byte-identical. Edges differ by 58 rows (0.19%).**
Both arms are individually 100% deterministic (re-run multiset diff = 0 for each).
Every divergence was traced to one of three explained causes (below); only one
aggregate kind (`instantiates`, +3.1%) exceeded the 2% bar, and there the **Rust
output is the correct one** — the TS arm loses edges to a pre-existing upstream bug.

## Count tables

### Nodes by kind — IDENTICAL (row-level set diff = 0, including sha256 IDs)

| kind | TS | Rust |
|---|---|---|
| class | 50 | 50 |
| constant | 481 | 481 |
| enum | 39 | 39 |
| enum_member | 205 | 205 |
| file | 351 | 351 |
| function | 3152 | 3152 |
| import | 1775 | 1775 |
| interface | 104 | 104 |
| method | 2170 | 2170 |
| property | 2 | 2 |
| struct | 207 | 207 |
| trait | 9 | 9 |
| type_alias | 38 | 38 |
| variable | 522 | 522 |
| **total** | **9105** | **9105** |

The node-ID set (sha256-derived) is exactly equal between arms — hashing and
qualified-name construction are bit-parity.

### Edges by kind

| kind | TS | Rust | Δ | Δ% |
|---|---|---|---|---|
| calls | 15503 | 15438 | −65 | 0.42% |
| contains | 9507 | 9507 | 0 | — |
| extends | 13 | 13 | 0 | — |
| implements | 26 | 26 | 0 | — |
| imports | 1772 | 1770 | −2 | 0.11% |
| instantiates | 255 | 263 | +8 | **3.14%** |
| references | 4094 | 4095 | +1 | 0.02% |
| **total** | **31170** | **31112** | **−58** | **0.19%** |

Row-level multiset diff: **178 rows only-TS, 120 rows only-RS** (~0.6% of edges).

### Files — IDENTICAL (row-level path|language set diff = 0)

| language | TS | Rust |
|---|---|---|
| javascript | 18 | 18 |
| rust | 146 | 146 |
| typescript | 187 | 187 |
| yaml | 2 | 2 |
| **total** | **353** | **353** |

### Unresolved references

| | TS | Rust | Δ |
|---|---|---|---|
| `unresolved_refs` rows | 39,815 | 39,846 | +31 (0.08%) |

## Divergence analysis (every differing edge row classified)

### 1. Ambiguous-name tie-breaks — 85 rows on EACH side (paired; net 0)

Same call site, same candidate set, different winner among equally-scored
same-named definitions. Examples:

- 27× `git(...)` calls in `__tests__/extraction.test.ts` (the real def is a local
  `const git = (...) =>` arrow at line 3538 that neither arm indexes as a function;
  6 same-named candidates exist): TS picks `__tests__/sync.test.ts:223`, Rust picks
  `__tests__/worktree-detection.test.ts:27`. Both equally heuristic; neither correct.
- `get_outgoing_edges` calls in fixture Rust sources: TS resolves to
  `CodeGraph::get_outgoing_edges`, Rust to `QueryBuilder::get_outgoing_edges`.
- `log_warn`, `Daemon::start`, `CodeGraph::open` vs `DatabaseConnection::open`, etc.

**Classification: (c) acceptable difference** — candidate ordering between
equal-confidence matches differs (JS Map insertion order vs Rust map/sort order).
Neither arm's pick is more correct; counts are unaffected.

### 2. Batch-boundary duplicate-reference loss — TS loses 35 edges, Rust loses ~57

**This is a pre-existing upstream TS bug, faithfully ported — both arms have it; it
hits different victims because their `unresolved_refs` insertion order differs.**

Mechanism: `resolveAndPersistBatched` (TS `src/resolution/index.ts:735`; Rust
`rust/src/resolution/resolver.rs:1012`) pages `unresolved_refs` by rowid in
5000-row batches and, after each batch, calls `deleteSpecificResolvedReferences`
(TS `src/db/queries.ts:1729`; Rust `rust/src/db/queries.rs:2025`) which deletes by
`(from_node_id, reference_name, reference_kind)` **without a line predicate**.
When duplicates of the same key straddle a page boundary, the post-boundary rows
are deleted before keyset pagination (`id > lastSeenRefId`) ever reads them — their
edges are silently never created.

Proven victims:

- **TS arm** (explains the only >2% aggregate): `__tests__/frameworks-integration.test.ts`
  has 18 `CodeGraph.initSync(...)` sites (verified: the TS extractor emits all 18 refs
  when run standalone); TS creates `instantiates` edges only for the first 10 (lines
  37–574), losing the 8 at lines 595–888. Rust gets all 18. Plus 13 lost `calls` in
  `pr19-improvements.test.ts` (`cleanupTempDir`×4, `createTempDir`×3, `destroy`×3,
  `initSync`×3), 3 `imports`→`crate` from `rust/src/extraction/languages/scala.rs`,
  2 `glob_to_regex`, 5 `calls` from `main`, and singles — 35 total.
- **Rust arm**: 39 of 234 `extractFromSource(...)` calls in `__tests__/extraction.test.ts`
  (TS resolves 234/234, Rust 195/234), 9 `detectLanguage` (44 vs 35), 4
  `isLanguageSupported`, 3 `getSupportedLanguages`, 5 `imports`→`crate` from
  `rust/src/extraction/orchestrator.rs` (TS emits 15 `use crate::` edges, Rust 10),
  and singles — ~57 total.

Confirmation it is loss-not-resolution: the missing rows are absent from
`unresolved_refs` too (deleted unprocessed), and each arm reproduces its own losses
byte-identically on re-run.

**Classification: (b) TS bug** (inherited by the port). Fix belongs upstream: make the
post-batch delete line-aware (or delete only the exact rowids of the batch).

### 3. Name-matcher receiver-type-inference gap — Rust misses ~33 `calls` edges

For member refs in fixture Rust sources whose receiver is a short local variable —
`t.len`, `m.start`, `full.start`, `trimmed.len`, `parts.len`, etc. — the TS
name-matcher's source-scanning receiver-type inference resolves them (e.g.
`get_stem_variants`'s `t.len` × 10 → `OrderedNodes::len`), while the Rust port
leaves them **unresolved** (the rows remain in Rust's `unresolved_refs`, proving
this is a resolution-behavior difference, not batch loss). This is also the bulk of
the +31 unresolved-count delta.

Note on correctness: the TS edges in this class are wrong-target junk — `t` in
`get_stem_variants` is a `&str`, so `t.len()` is `str::len`, not `OrderedNodes::len`.
Rust's conservatism produces a *more* correct graph here, but it is still a
behavioral divergence from the reference implementation.

**Classification: (a) Rust-port divergence** (strict-parity view) /
intentionally-acceptable (quality view). ~33 edges, 0.2% of `calls`.

### 4. Extraction ref-name shape for Rust field calls — no edge impact

At identical sites/lines (e.g. tree-sitter API calls in the fixture's Rust sources),
TS emits the bare member (`child_count`, `named_child_count`, `start_byte`, `rfind`,
`end_byte`), Rust emits receiver-qualified (`node.child_count`, `child.child_count`, …).
All affected refs are unresolved in BOTH arms (the names don't exist in the fixture),
so no edges differ — only the *text* of ~60+ unresolved rows. Also 3 rows where TS
records `parseWorker as import` vs Rust `import` for an aliased import ref.

**Classification: (c) acceptable** (cosmetic, unresolved-only), though worth aligning
if bit-parity of `unresolved_refs` is ever wanted.

## Functional parity spot checks

Same inputs against each arm's own index (TS index in a sibling copy, Rust index in
the fixture):

| check | inputs | result |
|---|---|---|
| `query <term> --json` | `ExtractionOrchestrator`, `resolveViaImport`, `synthesize` | **Identical** after stripping `updatedAt` (index timestamps necessarily differ) and null-valued keys: TS serializes `"visibility": null`, Rust omits null fields. All IDs, names, paths, line numbers, ordering equal. |
| `affected <file> --json` | `src/extraction/tree-sitter.ts`, `src/db/queries.ts`, `rust/src/resolution/name_matcher.rs` (+ multi-file `src/index.ts src/utils.ts`) | **Byte-identical** (all return empty `affectedTests` on this fixture in both arms — the fixture yields no file→file import edges; identical behavior). |
| `callers resolveViaImport --json` (bonus, non-trivial traversal) | — | **Identical** after the same normalization. |

Node IDs are sha256 of the same strings in both arms — confirmed by the exact
9,105/9,105 ID-set match and ID-equality in all CLI outputs.

CLI surface (`--help` for root, `init`, `index`, `query`, `affected`) matches
command-for-command and flag-for-flag (commander vs clap formatting aside).

## Verdict

Within tolerance. The only >2% aggregate (`instantiates` +3.1%) is the upstream TS
batch-boundary bug where **Rust produces the correct count**. Real action items:

1. (Upstream TS + port, both affected) fix `deleteSpecificResolvedReferences` /
   `resolveAndPersistBatched` duplicate loss across page boundaries.
2. (Rust port, strict parity only) decide whether to reproduce the TS name-matcher
   receiver-type-inference for `var.member` refs (~33 edges; TS's matches in this
   class are wrong-target, so divergence may be desirable).
3. (Cosmetic) `query --json`: TS emits `"visibility": null`, Rust omits null keys.

## Intentional MCP divergences (rmcp gap fixes — 2026-06-06)

The MCP server deliberately EXCEEDS the TS parent on spec MUSTs/SHOULDs the TS
arm lacks (full gap matrix + rationale: `notes/rmcp-gaps.md`, "Implemented
divergences"). Every site is marked `// EXCEEDS TS:` in source. Summary:

1. **Proxy degraded mode answers every request** — TS `proxy.ts::handleLocally`
   answers only `tools/call` + `ping` and silently drops everything else (host
   hangs until its own timeout). Rust `proxy.rs::handle_locally` adds: `-32601`
   `"Method not found: {method}"` for any other id-bearing request, `-32700`
   `"Parse error: invalid JSON"` (`id:null`) for unparseable lines, `-32600`
   for valid-JSON non-JSON-RPC, static `tools/list`, and a `{}` ack for
   `logging/setLevel`; notifications stay dropped. Error strings/wire shape are
   byte-identical to the session transport (`transport.rs::ErrorCodes`).
2. **`notifications/initialized` recognized by its spec name** — TS
   `session.ts:140` matches only the bare legacy `"initialized"`; Rust matches
   both spellings in the dedicated no-op arm.
3. **Tool `annotations`** — all 8 tools advertise
   `{readOnlyHint:true, destructiveHint:false, idempotentHint:true,
   openWorldHint:false}` (TS ships none).
4. **`tools.listChanged`** — capabilities are
   `{"logging":{},"tools":{"listChanged":true}}` (TS: `{"tools":{}}`);
   `notifications/tools/list_changed` is emitted when a project open changes
   the gated/budgeted list; the local-handshake proxy forwards `tools/list` to
   the daemon once Ready (TS answers statically forever) and nudges the client
   with `list_changed` after attaching if it had answered statically.
5. **Progress** — `tools/call` honors `_meta.progressToken` (string/int);
   the catch-up sync emits `notifications/progress` through it. Never
   unsolicited (TS ignores the token).
6. **Cancellation honored** — `notifications/cancelled` (intercepted on the
   reader thread) sets a per-request cancel flag checked between engine
   pipeline stages; the response of a cancelled call is suppressed (TS only
   tolerates the notification).
7. **`logging` capability** — `logging/setLevel` stores a per-session min
   level; `[CodeGraph MCP]` engine/session diagnostics are mirrored as
   `notifications/message {level, logger:"codegraph", data}` (stderr bytes
   unchanged).
8. **`notifications/roots/list_changed`** — re-arms the one-shot roots/list
   latch while no project is resolved (TS drops it).
9. **Timeout cancellation** — our own `roots/list` timeout now emits
   `notifications/cancelled {requestId, reason:"request timeout"}` before
   abandoning the pending entry.

These do not change any TS-parity wire bytes asserted by the suite
(serverInfo key order, error strings, tool response shapes,
`SERVER_INSTRUCTIONS`); only additive fields/messages and previously-absent
responses differ.

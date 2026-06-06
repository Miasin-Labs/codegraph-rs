# Tier-1 CLI close work — needed engine/bridge APIs + workarounds in place

Date: 2026-06-06. Written by the Tier-1 CLI agent (owner of `src/analyze.rs`,
`src/bin/codegraph.rs`, `tests/analyze_cli_test.rs`). Coordination point with
the engine agent (`notes/close-engine.md`). Every item below has a working
host-side workaround — nothing blocks the Tier-1 surface — but each names the
engine/bridge change that would let the workaround be deleted.

## 1. `co_change::fetch_git_history` / `parse_git_log` mis-parse real git output (BUG)

`fetch_git_history` runs `git log --name-only --format=%H` and feeds it to
`parse_git_log`, which expects blocks shaped `hash\nfiles…\n<blank>`. Real
git emits `hash\n<blank>\nfiles…` with **no** blank before the next hash, so
the parser takes the blank after the hash as end-of-block, then mistakes the
first file for the *next commit's hash* and appends the next hash to the
file list. Result: every commit's first file is dropped and a bogus
"hash-as-file" pollutes pairs — verified live (cross-file pairs came back 0
on a fixture where two files were committed together 3 times). The engine's
own unit tests pass because they hand-craft the expected (wrong) format.

- Host workaround: `analyze::fetch_commit_history` (private, in
  `src/analyze.rs`) shells `git log --name-only --format=%x1e%H` and splits
  on the `\x1e` record separator, then feeds the engine's public
  `CommitInfo` structs into `compute_co_changes` / `co_changes_for_nodes`
  (which are correct and are what `analyze co-change` uses).
- Engine fix wanted: make `parse_git_log` parse the real `--format=%H`
  shape (hash, blank, files) or switch `fetch_git_history` to a
  record-separated format. Then the host fetcher can be deleted.
  Note the DSL `co_changes` operator shells git through the same broken
  pair, so it under-reports today as well.

## 2. `schema::Envelope`'s `kind` is the closed 4-variant `PayloadKind`

Matrix item 12 ("--json everywhere via schema::Envelope") cannot reuse the
engine type for host reports: `PayloadKind` has exactly QueryResult /
EntrypointSummary / ContextResult / FormattedOutput, and `Envelope<T>`'s
`kind` field is that enum.

- Host workaround: `analyze::ReportEnvelope` mirrors the wire shape with an
  open `&'static str` kind and camelCase keys
  (`{"schemaVersion": N, "kind": "<report>", "data": …}`); every
  `codegraph analyze … --json` is wrapped in it. `analyze schema <kind>`
  prints the engine's `json_schema_for` documents for the four engine kinds
  verbatim.
- Engine change wanted (optional): a string-kind envelope variant (or
  `PayloadKind::Other(String)`) if the engine wants to own host report
  schemas too. Low priority — the mirrored envelope is stable and versioned
  host-side (`analyze::REPORT_SCHEMA_VERSION`).

## 3. `capabilities::ALL_CAPABILITIES` is private; no dependency accessor

`analyze capabilities` needs to iterate every capability and show the
dependency cascade. The array is a private const and
`CapabilityTree.dependencies` has no public reader.

- Host workaround: the 6 variants are mirrored in `src/analyze.rs`
  (`ALL_CAPABILITIES`), and the cascade is probed behaviorally — disable
  each capability on a fresh `CapabilityTree::new()` and record what
  `disable()` reports as cascaded. Goes stale if the engine adds a 7th
  variant (compile error will flag it only if the enum stays exhaustive).
- Engine change wanted: `pub fn all() -> &'static [Capability]` and
  `pub fn dependencies(&self, cap: Capability) -> &[Capability]` (or an
  iterator over the tree).

## 4. Bridge metadata enrichment to light up boundaries + generics (bridge-owner item)

Two engine analyses read well-known metadata keys the bridge does not
populate, so over a bridged index they are honestly-empty (both commands
print capability notes instead of silent zeros):

- `polyglot::detect_*` reads `http_route`/`http_method`,
  `http_client_target`/`http_client_method`, `ffi_export`, `wasm_export`,
  `wasm_import_module`+`wasm_import_name` on Function nodes. The bridge
  drops host `route` nodes (5-kind projection) and host signatures don't
  carry `extern "C"`/wasm qualifiers (verified: the Rust extractor's
  signature is params+return only). Folding host route nodes into handler
  metadata at bridge time would make `analyze boundaries` real.
- `monomorphize::find_instantiations` reads `generic_params` (decl) +
  `callee_type_args` (caller). Neither is extracted nor bridged. Until
  then `analyze generics` lists signature-heuristic
  `likelyGenericDefinitions` and reports the instantiation gap in its note.

## 5. FYI — `validation::validate_signature_change` verdict granularity

The validator marks **every** direct caller incompatible whenever
`old_param_count != new_param_count` (it has no per-call-site argument
counts to compare). `analyze validate` states this in its `note` field
rather than implying per-call-site precision. Once byte ranges / IR land
(close-list #17), per-call-site arg counts could make the verdicts real.

## Consumed from `notes/close-engine.md`

- `analysis::dominator_tree` batch API — `analyze dominators` migrated off
  the per-node `dominator_chain` loop as instructed (§3 of that note).
- `reachable via "<pattern>"` DSL operator — rides `analyze query` for
  free; mentioned in the query help text.
- Partial-struct registration APIs (§2) — not consumed here; they are
  bridge/context surfaces, not Tier-1 CLI items.

# Structured output, replayed (2026-09-26)

Question: the nine graph tools that returned markdown (`callers`, `callees`,
`impact`, `tests`, `history`, `diagnostics`, `arch`, `xref`, `paths`) now
declare an output schema and send compact JSON. Does the bigger JSON pay for
itself? Aggregates only; the calls and outputs stay in the local
session-mining store.

## Method

- **Calls:** the real agent calls to those tools in the mined corpus (792).
  Kept: the ones whose project still exists on a current-schema index, or on
  a scratch copy of two older ones (the originals were never opened by a
  newer binary). After de-duplication, 100 recorded calls (64 `arch`,
  24 `xref`, 7 `callers`, 3 `callees`, 2 `paths`), plus 128 derived ones:
  `callers`/`callees`/`impact`/`tests` on every symbol the agents asked about.
- **Builds:** `5dfd33c` (markdown) and `da821f4` (structured, before the
  impact-radius fix, so only the encoding differs), each as
  `codegraph serve --mcp` with a scratch home, no watcher, no sync.
- **Measured:** the text a model receives: bytes, and whether both name the
  same files and identifiers.
- **Reading test:** 20 questions with exact answers (counts, files, lines)
  over 20 replayed results. Fresh agents answered them blind, one format
  each, on Sonnet and Haiku.

## Results

| tool | calls | markdown | JSON | size | content |
|---|---|---|---|---|---|
| arch (recorded) | 64 | 590 KB | 661 KB | +12% (median +36%) | 23 budget-capped calls list fewer files (counted in `filesOmitted`) |
| xref (recorded) | 24 | 17 KB | 24 KB | +41% | same |
| callers / callees | 74 | 40 KB | 57 KB | +36–48% | same |
| impact (derived) | 32 | 31 KB | 54 KB | +76% | same |
| tests (derived) | 32 | 13 KB | 15 KB | +11% (median −38%) | same |
| paths (recorded) | 2 | 0.4 KB | 0.3 KB | −8% | same |
| **all** | 228 | 690 KB | 811 KB | **+17%** | no errors on either build |

Reading accuracy (questions right): Sonnet 20/20 markdown, 20/20 JSON;
Haiku 20/20 markdown, 19/20 JSON. The miss was counting a 20-row callers
list, which the markdown's header had stated ("20 found") and the JSON left
to be counted.

## Conclusions

- **The JSON does not read better; it reads the same.** Its payoff is
  that machines can use it: the SDK's rows, schema validation, and one
  format across tools. That costs about a sixth more text, most of it in
  `impact` and `callers`.
- **Where the markdown stated a total, state it.** Callers/callees now
  carry `count` (the one reading miss).
- **Rows follow the lean-row rule.** `arch` dropped its per-file
  `language` (the extension says it); that saved about 2.5%. The rest of the
  overhead is key names per row, the price of self-describing rows.
- **Budget-capped `arch` fits fewer files in JSON.** It is opt-in and
  says how many it left out. If agents come to rely on it, a denser
  per-file symbol encoding is the lever.

# Compiler detector (`analyze bugs --detector compiler`)

rustc and clippy run on HIR with real types, trait impls and macro
expansion — what tree-sitter cannot see. The compiler detector runs them and
brings their findings into `analyze bugs`, where they get the graph's
context (who reaches the code), ranking, suppression and review packets.

## Pipeline

1. `crate::diagnostics::clippy_lints_or_poll` runs
   `cargo clippy --offline --message-format=json --workspace -- <args>`
   detached under `.codegraph/diagnostics/compiler.*`, waits at most
   `--compiler-wait` (300 s default), and a later call picks up a run still
   going. Default targets only (lib + bins): test targets need
   dev-dependencies an offline cache often lacks, and test code is filtered
   anyway. `CARGO_TARGET_DIR` is honoured.
2. Arguments (`lints::driver_args`): `-A clippy::all`, then `-W` the
   `correctness` and `suspicious` groups and every table lint (deny-level
   ones downgraded so one hit does not stop the crate), then `-A` the group
   members that are documentation/tooling hygiene. `#[allow]` in source still
   wins; a `[lints]` table in Cargo.toml does not (command-line flags follow
   it).
3. Parsing (`diagnostic.rs`): the curated lints only; the place is the
   outermost macro call site inside the project when the primary span is in a
   macro body; spans outside the indexed files (dependencies, std,
   `target/…/out`) are dropped; children and labelled spans become evidence,
   the per-lint boilerplate does not; `--workspace` duplicates collapse.
4. Cache (`cache.rs`): a finished run is stored under a key of the
   lockfile's content, every indexed file's size+mtime, the build files and
   the arguments — keyed *after* the run (cargo may write `Cargo.lock`), and
   not stored when a source changed after the run started (`stale`).
5. Findings (`mod.rs`): rule `clippy::<lint>` / `rustc::<lint>`, the
   enclosing indexed function, col 0-based. `unwrap_in_result` absorbs
   `unwrap_used`/`expect_used` on its line. `shapes.rs` drops lock unwraps,
   literal unwraps and `x += 1`, and discounts additions, string-keyed
   indexing and 32-bit-only truncation (every reviewed finding of those
   shapes was a false positive).
6. Reach (`reach.rs`): entry points are route handlers (index `route` →
   `references` → fn; a handler resolved to a `self` method stands for the
   free fn of that name, since the route scan keeps only the last path
   segment), then fns taking request extractors (`Json<`, `Query<`,
   `Multipart`…), else — a library — public fns taking bytes/text/readers
   (all public fns if none). A BFS over resolved call edges (≤8 calls)
   records the nearest entry and the path. An input-exposure lint rises
   toward `2.5 × base` (≤0.9) at a handler, ×0.85 per call, halfway near a
   public API, and halves where nothing reaches; other lints +0.05 near a
   handler.
7. Report: `BugsReport.compiler` (`state` complete|running|unavailable,
   `cached`, `stale`, `compileErrors`, `failure`, entry kind/count).
   `analyze review --detector compiler` adds the lint's questions and a
   "Reachability: route … → …" question.

## Lint table and measured precision (2026-09)

Measured on rms (hand-judged, 360 findings after shapes, top 50 read),
codegraph-rs (4,802 findings, top 25 read) and rustsec-adjacent (306
vuln/fixed pairs; **184 of 612 units built offline** — 176 cleanly, 8 with
compile errors — and **84 pairs built on both sides**; the rest fail offline
resolution without a lockfile, or ship a manifest naming excluded
examples/benches). "TP" = fires in the fix region and is gone in `fixed/`;
"region" = all findings in the fix region.

| lint | class | RustSec TP / region / all vuln | rms / self verdict |
|---|---|---|---|
| `undocumented_unsafe_blocks` | Low | 17 / 90 / 4,281 (11 pairs) | lead only |
| `arithmetic_side_effects` | Rare | 7 / 165 / 2,052 (4 pairs) | 0 real in top 50 (counters, sums) |
| `indexing_slicing` | Rare | 0 / 118 / 1,245 | 0 real (guarded; `Value["k"]`) |
| `cast_possible_truncation` | Rare | 0 / 17 / 812 | 1 minor real (rms `u64 as u32` of a PATCH field) |
| `unwrap_used` | Rare | 2 / 146 / 799 | 0 real (locks, literals, DB rows) |
| `expect_used`, `panic` | Rare | 0 / 129 / 1,337 | 0 real |
| `string_slice` | Low | 2 / 91 / 169 | 0 real (offsets from ASCII `find`) |
| `unwrap_in_result` | Low | 2 / 13 / 107 | 0 real |
| `uninit_vec` (correctness) | High | 2 / 4 / 6 | — |
| `not_unsafe_ptr_arg_deref`, `read_zero_byte_vec` | High | 1 / 1 each | — |
| `suspicious_op_assign_impl` | Low | 0 / 0 / 20 | — |
| other correctness / suspicious | High / Medium | ≤5 findings each, none at a fix | none fire (both run clippy in CI) |
| rustc `unused_must_use`, `dropping_references` | Medium | 0 / 0 / 8 | — |

Takeaways: the correctness group is precise but rare in real crates (both
codebases already run clippy); the restriction lints the detector exists to
rank are overwhelmingly guarded code, and reachability narrows them without
making them precise, so a reached rare lint (≤0.375) stays below any
correctness finding (0.8). `undocumented_unsafe_blocks` is the one
restriction lint that tracks RustSec fixes (a fix usually rewrites the
unsafe block it names).

## Clippy vs codegraph's rules

Clippy already covers, precisely, what these codegraph rules approximate for
Rust — they stay because they need no build and work on every language:

- `self-comparison` ↔ `eq_op`; `identical-branches` ↔ `if_same_then_else`,
  `ifs_same_cond`, `match_same_arms`; `loop-no-progress` ↔
  `while_immutable_condition`, `infinite_loop`; `constant-condition` ↔
  `absurd_extreme_comparisons`, `impossible_comparisons`; `dead-store` ↔
  rustc `unused_assignments`; `result-discarded` ↔ `unused_must_use` (for
  `#[must_use]` types); `comparison-discarded` ↔ `no_effect`.
- YAML: `rust-set-len-on-uninit` ↔ `uninit_vec`, `rust-uninit-assume-init`
  ↔ `uninit_assumed_init` / rustc `invalid_value`.

No clippy counterpart (keep): `arm-result-deviance`,
`missing-companion-call` (beliefs mined from the project), the Rudra-style
YAML rules (`rust-panic-unsafe-ptr-read`, `rust-drop-in-place-before-set-len`,
`rust-vec-from-raw-parts-swapped`, `rust-send-sync-without-bound` — clippy's
`non_send_fields_in_send_ty` is nursery and narrower —,
`rust-transmute-lifetime-extension`, `rust-get-unchecked-unvalidated`,
`rust-from-utf8-unchecked-on-input`, `rust-alloc-unchecked-null`,
`rust-bytes-of-generic-value`), `rust-length-product-overflow` (the targeted
form of `arithmetic_side_effects`), and every `rust-server.yaml` rule.

Clippy lints with no codegraph rule, as ideas — but most need types, so run
clippy rather than port them: `await_holding_lock`, `let_underscore_future`,
`unused_io_amount`, `mutable_key_type`, `path_buf_push_overwrite`,
`join_absolute_paths` (path traversal), `suspicious_open_options`,
`nonsensical_open_options`, `non_octal_unix_permissions`,
`permissions_set_readonly_false`, `zombie_processes`,
`suspicious_command_arg_space`, `read_line_without_trim`,
`char_indices_as_byte_indices`, `suspicious_splitn`. Syntax-only candidates
worth a cross-language rule: octal-looking permission literals, a
`read_line` result compared without trimming, `step_by(0)`.

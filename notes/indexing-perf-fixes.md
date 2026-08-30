# Indexing performance + hang-safety fixes (2026)

Context: `codegraph index` wedged at ~60% ("Parsing code") on a large C++ repo
(AyuGram Desktop, 14,392 files). Root-caused to CPU-bound quadratic work in
extraction/resolution, not a deadlock. Fixes below, each benchmark-backed.

## 1. C/C++ `get_visibility` was O(members^2)  [FIXED]

`get_visibility` scanned the parent class/struct body per member with
`parent.child(i)`, and `tree_sitter::Node::child(i)` re-walks the child list
from the start each call (documented "cost is technically log(i) ... use
`Node::children` instead"). A body with N members was O(N^2). On a
macro-confused parse or a giant aggregate (tens of thousands of siblings under
one node) this is minutes of pure-CPU spin — the hang.

Fix: `cpp_visibility_for` computes the parent's first-access-specifier verdict
once in a single O(children) cursor pass, memoized per parent in a thread-local
keyed by the tree root id (dropped when the root changes, i.e. per file — safe
because extraction is one-tree-at-a-time per worker thread).

Benchmark (synthetic C++ aggregate, per-member visibility resolution):
| members | per-member O(n^2) | one-pass O(n) | speedup |
|--------:|------------------:|--------------:|--------:|
|   1,000 |            83 ms  |      0.17 ms  |   ~500x |
|   8,000 |         6,045 ms  |      1.09 ms  |  ~5,566x|
|  25,000 |        60,465 ms  |      2.98 ms  | ~20,298x|

## 2. `child(i)` iteration loops swept to cursor walks  [FIXED]

48 `for i in 0..node.child_count() { node.child(i) }` loops across the language
extractors were converted to `for c in node.children(&mut cursor)` (O(1) per
step vs O(i)). Strict improvement, identical semantics. (Two nested/labeled
loops — kotlin, objc — needed hand-fixing after the mechanical pass.)

## 3. Resolver `read_file` cloned whole files per reference  [FIXED]

The snapshot resolution context's `read_file` returned an owned `String` (a full
file-body copy) on every call, and C/C++/Go/local receiver-type inference calls
it once per reference and re-splits into lines each time. On AyuGram there are
202,593 dotted C/C++ member refs; `history_widget.cpp` alone has 2,219, each
re-cloning a ~500 KB file.

Fix: `file_cache` now stores `Arc<str>`; a new `read_file_arc` returns a
refcount bump instead of a body copy. Hot consumers (cpp/local/go receiver
inference) use it. `read_file` (owned) is kept for the few callers that need a
String.

Benchmark (540 KB file, 2,200 refs): clone+resplit-per-ref 237 ms →
Arc + cached line index 0.05 ms (~4,395x).

## 4. `MAX_FILE_SIZE` guard — available as OPT-IN (default disabled)  [ADDED]

The TS reference (`src/extraction/index.ts`) keeps a 1 MiB cap that skips
oversized files with a `size_exceeded` warning. This crate, however,
deliberately indexes every file regardless of size — a tracked file that grows
past 1 MiB is re-indexed, not dropped (enforced by the `git_based_sync`
integration test). Because the quadratic blow-up is now fixed at the algorithm
level (items 1-3), no default cap is needed.

The cap is therefore wired as an **opt-in**: `DEFAULT_MAX_FILE_SIZE = 0`
(disabled). Set `CODEGRAPH_MAX_FILE_SIZE=<bytes>` to skip larger files with a
`size_exceeded` warning (bulk + single-file paths); `0`/unset/invalid = no cap.
This respects the project's "no size cap" design while giving operators indexing
hostile trees an escape hatch.

## 5. Cooperative-yield + main-thread liveness watchdog — INTENTIONALLY NOT PORTED

The TS `cooperative-yield.ts` + `liveness-watchdog.ts` (issues #850/#1091) exist
because JS is single-threaded: a non-yielding loop on the main thread wedges the
event loop so that timers, signal handlers, and the PPID watchdog — all running
*on* that loop — cannot fire, pinning a core forever with no self-recovery. Their
fix is a separate watchdog PROCESS that SIGKILLs the wedged parent, plus periodic
`setImmediate` yields so a slow-but-progressing index isn't misclassified as
wedged.

This does not apply to the Rust port's architecture:
- Parsing and reference resolution run on Tokio `spawn_blocking` worker threads
  (`parse_batch`, `resolver/parallel.rs`), NOT the async runtime. A quadratic
  extraction loop pins one worker thread; the runtime, signal handling, and MCP
  responsiveness are unaffected — there is no event loop to starve.
- The MCP daemon already has a PPID watchdog (`mcp/proxy/watchdog.rs`,
  `mcp/server/direct.rs`) for host-death liveness, which is the part that is
  actually relevant cross-runtime.
- The real cause of the observed wedge was the O(n^2)/O(n·files) CPU work above,
  now removed. A watchdog would have masked that by killing a valid index.

Progress reporting for the resolving phase is per-batch
(`resolve_and_persist_batched`, default 5,000 refs) and per-chunk inside
`resolver/parallel.rs`, so the CLI bar advances during long resolution.

Conclusion: porting the main-thread liveness watchdog would be cargo-culting a
JS-runtime workaround into a threaded Rust runtime where it is unnecessary. The
algorithmic fixes remove the wedge at the source.


## 6. `memory-budget.ts` + resolver worker-pool — INTENTIONALLY NOT PORTED

TS `memory-budget.ts` computes cgroup/reclaim-honest available memory to
(a) size the resolver WORKER POOL (`resolver-pool.ts` spawns separate worker
*processes*, each holding its own ~1 GB copy of the index — sizing by cores
alone OOM-killed a kernel-scale index in a 7 GB container) and (b) cap the
c-fnptr synthesizer cache.

Not applicable to the Rust port:
- Resolution runs as `spawn_blocking` tasks over a single shared
  `Arc<SnapshotContext>` — one process, one heap, no per-worker index copy. The
  N×~1 GB duplication that motivated the memory budget does not exist here.
- Worker count comes from `std::thread::available_parallelism()`, which on
  modern std IS cgroup/cpuset-aware on Linux, so CPU oversubscription in a
  capped container is already avoided.
- The c-fnptr synthesizer builds per-file `HashMap`s bounded by file content,
  not the cross-file growing cache (24,576-entry) that the TS memory cap guards.

If a future change reintroduces separate resolver processes or a large
cross-file resolver cache, port `memory-budget` then. Until then it would add a
platform-specific dependency with no benefit.

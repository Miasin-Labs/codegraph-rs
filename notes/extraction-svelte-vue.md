# svelte+vue extractor port notes

Ported files:

- `src/extraction/svelte_extractor.rs` ← `src/extraction/svelte-extractor.ts` (323 ln, incl. template-call extraction, #629 template component usages, Svelte 5 rune filtering)
- `src/extraction/vue_extractor.rs`    ← `src/extraction/vue-extractor.ts` (290 ln, incl. #629/#657/#659 `<template>` component usages: PascalCase + kebab→Pascal, Vue built-ins skip-list)

Parity was validated against the **actual TS implementation** (the built
`dist/extraction/{svelte,vue}-extractor.js` run via node on identical
fixtures); every line/column/node-kind expectation in the unit tests is a
captured TS output vector, including the TS line-offset skew for script-block
symbols (a node on file line 2 is reported at line 3 when the `<script>` tag
is on line 1 — the TS arithmetic `contentStartLine = scriptTagLine +
openingTagLines + 1` double-counts the leading newline of the block content;
reproduced exactly) and one exact `generateNodeId` hash
(`component:6fc2ade5f7dcfd79726083ba2bf1203a` for `Static.svelte`).

## Public API surface

```rust
// svelte_extractor.rs
pub type ScriptExtractorLookup =
    fn(Language) -> Option<&'static dyn LanguageExtractor>;

pub struct SvelteExtractor<'a> { /* private */ }
impl<'a> SvelteExtractor<'a> {
    pub fn new(file_path: impl Into<String>, source: &'a str,
               script_extractor_lookup: ScriptExtractorLookup) -> Self;
    pub fn extract(mut self) -> ExtractionResult; // consumes self (TS: extract() once)
}

// vue_extractor.rs (uses svelte_extractor::ScriptExtractorLookup)
pub struct VueExtractor<'a> { /* private */ }
impl<'a> VueExtractor<'a> {
    pub fn new(file_path: impl Into<String>, source: &'a str,
               script_extractor_lookup: ScriptExtractorLookup) -> Self;
    pub fn extract(mut self) -> ExtractionResult;
}
```

## Integration / wiring requirements

1. **`ScriptExtractorLookup` injection** — the TS constructors were
   `new SvelteExtractor(filePath, source)` and looked up `EXTRACTORS[lang]`
   internally inside `TreeSitterExtractor`. Natively `TreeSitterExtractor::new`
   takes the per-language config from the caller (see
   `notes/extraction-core.md`), so these extractors take a lookup fn. The
   `extract_from_source` dispatcher must call:
   ```rust
   SvelteExtractor::new(path, source, languages::extractor_for).extract()
   VueExtractor::new(path, source, languages::extractor_for).extract()
   ```
   (`languages::extractor_for` was not yet landed when this port was written
   — the `languages/` task was in flight — hence the injection rather than a
   direct call. If the integrator prefers TS-shaped 2-arg constructors, add
   thin wrappers once `languages/` is stable.)
2. `extraction/mod.rs` already declares both modules; the TS `index.ts`
   re-exports `SvelteExtractor` / `VueExtractor` — mirror that in the mod.rs
   re-export pass. Consider also re-exporting (or relocating)
   `ScriptExtractorLookup`; it lives in `svelte_extractor.rs` only because I
   don't own `tree_sitter_types.rs` (its natural home).
3. The shared `#[cfg(test)] pub(crate) mod test_support` in
   `svelte_extractor.rs` holds a minimal TS/JS `LanguageExtractor`
   (copy of the reference `TsTestExtractor` from `tree_sitter_wrapper.rs`
   tests) used by both files' tests. Once `languages/` exports the real
   typescript/javascript extractors, tests could switch to
   `languages::extractor_for` — not required; the subset is sufficient for
   the asserted behavior.

## Deviations from TS (all behavior-neutral or noted)

- **No try/catch wrapper**: the TS `extract()` wraps the body in
  `try { … } catch` pushing `"Svelte extraction error: …"` (code
  `parse_error`) / `"Vue extraction error: …"`. Rust has no exceptions and no
  fallible call in the body (the inner `TreeSitterExtractor` reports errors
  via `ExtractionResult.errors`, ported separately), so the catch branch is
  unreachable and was dropped rather than emulated with `catch_unwind`.
- **Backreference regex expanded**: TS
  `/<(script|style)(\s[^>]*)?>[\s\S]*?<\/\1>/g` uses a backreference the
  `regex` crate doesn't support; expanded to the exactly-equivalent
  two-branch alternation `<script…>…</script>|<style…>…</style>`.
- **ASCII word boundaries**: JS `\b` is ASCII; Rust patterns use `(?-u:\b)`
  where the TS regex relied on it (template call/tag scanning, `\bsetup\b`)
  so non-ASCII neighbors behave identically.
- **Columns are UTF-8 byte offsets** (JS used UTF-16 code units) — identical
  for ASCII lines; same deviation as extraction-core.
- `endColumn` of the component node is the last line's **byte** length (JS
  `.length` = UTF-16 units).
- `is_module` (svelte) / `is_setup` (vue) are computed and stored on the
  script-block struct for parity but never read — same as TS (marked
  `#[allow(dead_code)]`).
- `extract_template_calls` keeps the unused `_script_blocks` parameter for TS
  signature parity.
- TS quirks preserved on purpose:
  - the script-block line-offset skew described above (faithful, not "fixed");
  - duplicate inner `file:` nodes — one per `<script>` block, same node id
    (DB upsert dedups later, exactly as in TS);
  - `if (edge.line)` / `if (error.line)` truthiness: a line of 0 is **not**
    offset;
  - `lang=["'](ts|typescript)["']` accepts mismatched quote pairs (`"ts'`) —
    same as the TS regex;
  - trailing-separator file paths fall back to the full path as component
    name (`split().pop() || filePath` "" falsiness).

## Tests (in-module; the TS suites were not separable)

`extraction.test.ts`'s Vue describe-block drives `extractFromSource` (the
dispatcher deferred to the wiring task), so its cases were ported as
in-module tests calling `VueExtractor` directly; assertions match the TS
expectations plus TS-probed line/column vectors. Svelte has no extraction
unit suite in TS (only resolution/framework integration tests, owned by
other modules); covered with equivalent in-module tests.

- svelte (4): template-only component node (+ exact TS node id),
  script-block offsets + template calls/`{expr}` scanning + component tags
  (`<Modal>`), rune filtering (`$state`/`$derived`) + control-flow skip
  (`{#if}`/`{:else}`/`{@html}`), multiline opening tag + two script blocks
  (`context="module"`).
- vue (9): component node from SFC, JS script functions,
  `<script setup lang="ts">`, #629 template usages (PascalCase + kebab +
  built-ins skipped + offsets), kebab built-in (`<keep-alive>`) skipped,
  dual script blocks, template-only file, containment edges,
  `kebab_to_pascal` unit.

NOT ported as unit tests here (depend on the full `languages/typescript.ts`
config, not on this module's logic): "calls from top-level <script setup>
initializers" (#425) and "calls from Vue Options API object methods" — they
exercise object-literal/method extraction inside the delegated TS extraction
and belong with the languages/dispatcher integration suite.

## Status at handoff

`cargo check`: full crate green, zero errors/warnings in my two files.
`cargo test --lib svelte` / `cargo test --lib vue_extractor`: **13/13 pass**
(4 svelte + 9 vue).

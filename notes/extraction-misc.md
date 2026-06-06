# extraction-misc port notes (standalone regex/manual extractors)

Ported files (all compile clean; 30 in-module unit tests pass):

- `src/extraction/liquid_extractor.rs`  ← `src/extraction/liquid-extractor.ts` (10 tests)
- `src/extraction/dfm_extractor.rs`     ← `src/extraction/dfm-extractor.ts` (8 tests)
- `src/extraction/mybatis_extractor.rs` ← `src/extraction/mybatis-extractor.ts` (6 tests)
- `src/extraction/ida_c_extractor.rs`   ← `src/extraction/ida-c-extractor.ts` (6 tests)

Test provenance: the Liquid tests mirror `__tests__/extraction.test.ts`
"Liquid imports"; DFM tests mirror "DFM/FMX Extraction" (incl. the full
`MainForm.dfm` fixture: 9 components / 5 handlers); IDA tests mirror the
"IDA C Extraction" describe (thunk, sub_*, params/locals/type-edges) minus
the `CodeGraph.init`-based resolution test (needs the public API, not yet
wired); MyBatis tests are the extractor-level half of
`__tests__/frameworks-integration.test.ts` MyBatis cases (qualified-name
contract `<namespace>::<id>`, `<include refid>` references, non-mapper XML
→ file node only). Tests construct the extractors directly because
`extract_from_source` (the dispatcher) is the wiring task's job.

## Public API surface (for the wiring wave)

```rust
// liquid_extractor.rs
pub struct LiquidExtractor<'a>;
impl<'a> LiquidExtractor<'a> {
    pub fn new(file_path: impl Into<String>, source: &'a str) -> Self;
    pub fn extract(self) -> ExtractionResult;
}

// dfm_extractor.rs
pub struct DfmExtractor<'a>;
impl<'a> DfmExtractor<'a> {
    pub fn new(file_path: impl Into<String>, source: &'a str) -> Self;
    pub fn extract(self) -> ExtractionResult;
}

// mybatis_extractor.rs
pub struct MyBatisExtractor<'a>;
impl<'a> MyBatisExtractor<'a> {
    pub fn new(file_path: impl Into<String>, source: &'a str) -> Self;
    pub fn extract(self) -> ExtractionResult;
}

// ida_c_extractor.rs
pub fn is_ida_generated_c(file_path: &str, source: &str) -> bool;
pub struct IdaCExtractor<'a>;
impl<'a> IdaCExtractor<'a> {
    pub fn new(file_path: impl Into<String>, source: &'a str, language: Language) -> Self;
    pub fn extract(self) -> ExtractionResult;
}
```

Constructor argument order matches the TS constructors 1:1. `extract()`
consumes self (same convention as `TreeSitterExtractor::extract`). Dispatcher
recipe (from notes/extraction-core.md, unchanged):
- `(lang == C || Cpp) && is_ida_generated_c(path, source)` → `IdaCExtractor::new(path, source, lang).extract()`
- `.liquid` → `LiquidExtractor`, `.xml` → `MyBatisExtractor`
- pascal + (`.dfm`|`.fmx`) → `DfmExtractor`

## Node/edge shape parity (load-bearing contracts preserved)

- Node IDs via `generate_node_id` with the same `{filePath}:{kind}:{name}:{line}`
  hash inputs — IDs match the TS build byte-for-byte. Exception (TS parity):
  the IDA **file** node id is the literal `"file:{filePath}"` (not hashed),
  and IDA type-alias `contains` edges reference that literal id.
- MyBatis statement nodes: `kind: method`, `language: xml`,
  `qualifiedName: "<namespace>::<id>"` — the suffix-match contract for the
  MyBatis framework synthesizer. `<sql>` fragments get signature `"<sql>"`;
  statements get `"VERB [param=X] [result=Y]"`. `<include refid="a.b.c">` →
  reference name `a::b::c`; bare refid → `<namespace>::<refid>`.
- Liquid: import node (`{path}::import:{name}`) + component node
  (`{path}::{render|include|section}:{name}`) per tag, file-level unresolved
  refs to `snippets/{name}.liquid` / `sections/{name}.liquid`, schema →
  `constant` node with 200-char docstring, `{% assign %}` → `variable`.
- DFM: `component` nodes (`{path}#{name}`, signature = Delphi type), nesting
  via a stack → `contains` edges, `OnX = Handler` → `references`
  UnresolvedReference from the enclosing component.
- IDA: one `function` node per file (comment `// Name:` > parsed signature >
  filename), `parameter`/`variable`/`type_alias` nodes, `calls`/`returns`/
  `type_of` unresolved references, local-variable cap at 2000 with the exact
  warning `IDA local variable extraction capped at 2000` / code
  `ida_local_limit`. File node has `isExported: false` (TS parity; the other
  three extractors leave it unset).

## JS-regex → Rust-regex rewrites (behavioral equivalence argued per case)

1. **Backreference** (mybatis): TS
   `/<(select|insert|update|delete|sql)\b([^>]*)>([\s\S]*?)<\/\1>/g` — the
   `regex` crate has no backreferences. Rewritten as an opening-tag regex +
   manual `find("</{elem}>")` for the close (the non-greedy `[\s\S]*?<\/\1>`
   finds exactly the first close tag), with the JS `exec`-loop resume
   semantics emulated: on a missing close tag the scan resumes at
   `open.start() + 1` (JS retries subsequent positions); on success it
   resumes after the close tag (JS `lastIndex`).
2. **Lookahead** (ida): TS `/\b(?:const|volatile)\s+(?=[*&])/g` → consuming
   `\b(?:const|volatile)\s+([*&])` replaced with `" $1"`. String-for-string
   identical output (the consumed `*`/`&` is re-inserted; the char class
   can never start another `const|volatile` match, so /g scan positions
   don't diverge).
3. `\w`/`\d` written as explicit ASCII classes (`[0-9A-Za-z_]`, `[0-9]`)
   because Rust `\w`/`\d` are Unicode-aware while JS's are ASCII.
4. `[\s\S]*?` kept verbatim (supported). Inline flags `(?i)`, `(?m)`,
   `(?im)` replace JS `/i`, `/m` flags. Lazy quantifiers are supported and
   the regex crate's leftmost-first capture semantics match the JS
   backtracking results for all patterns used here (no alternation tricks).

## Deviations / Node-isms dropped

- **Offsets are UTF-8 bytes, not UTF-16 code units** (same global deviation
  as extraction-core): columns, `end_column`, the IDA 16 KB detection sample
  (`slice(0, 16384)` → byte-truncated at a char boundary), and string-length
  arithmetic. Line numbers are unaffected (both sides count `\n` in
  consistent units). The 200-char truncations (`liquid` schema docstring,
  mybatis `previewSql`) count Rust `char`s vs JS UTF-16 units — identical
  except for astral-plane characters.
- **try/catch wrappers dropped as unreachable**: the TS `Liquid/DFM/MyBatis
  extraction error:` parse_error catch arms guard JS exceptions that cannot
  occur in the Rust port (the only fallible op, schema `JSON.parse`, is
  handled inline via `serde_json::from_str().ok()`). `errors` stays in the
  result (always empty for those three; IDA emits the local-cap warning).
- **Liquid schema `name` coercion**: a translation-object `name` resolves
  `en || Object.values(name)[0] || 'schema'` with JS truthiness — ported,
  including JSON-key insertion order (serde_json has `preserve_order` on).
  Non-string truthy picks: numbers/bools are stringified (serde_json number
  formatting may differ from JS for float-y JSON like `5.0` → "5.0" vs JS
  "5"); truthy objects/arrays fall back to `'schema'` where JS would
  produce `"[object Object]"` — degenerate inputs, noted for completeness.
- `path.basename`/`path.extname` (ida) reimplemented for forward-slash
  paths only (codegraph paths are normalized to `/`; the TS `path` module
  would additionally handle `\` on Windows).
- TS lazy `lineStarts` memoization (ida/mybatis) computed eagerly in `new()`
  — no behavioral difference.
- `JS lastIndexOf('\n', index-1)` quirk at `index == 0` in
  `is_in_line_comment` not reproduced (it cannot affect the boolean result;
  argued in-code).

## Integrator checklist

- [ ] `extract_from_source` dispatcher must route to these four (see
      notes/extraction-core.md "Deferred" recipe). All four are
      construct-then-`extract()`, no shared state, `Send`-safe for rayon.
- [ ] The TS test `extraction.test.ts` "should resolve IDA sub callers and
      callees after indexAll" needs the `CodeGraph` public API — port it in
      the wiring wave's integration tests.
- [ ] MyBatis Java↔XML bridging lives in `src/resolution/frameworks/mybatis.ts`
      (not mine); its synthesizer depends on the `<namespace>::<id>`
      qualified-name contract verified by my unit tests.
- [ ] No `mod.rs` changes needed — `extraction/mod.rs` already declares all
      four modules; if the barrel later re-exports types, add
      `LiquidExtractor`, `DfmExtractor`, `MyBatisExtractor`, `IdaCExtractor`,
      `is_ida_generated_c` (mirrors the TS `index.ts` surface).

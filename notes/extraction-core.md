# extraction-core port notes

Ported files (all compile clean; 24 in-module unit tests + 3 integration
tests pass):

- `src/extraction/tree_sitter_types.rs`  ← `src/extraction/tree-sitter-types.ts`
- `src/extraction/tree_sitter_wrapper.rs` ← `src/extraction/tree-sitter.ts` (TreeSitterExtractor only — see "Deferred")
- `src/extraction/tree_sitter_helpers.rs` ← `src/extraction/tree-sitter-helpers.ts`
- `src/extraction/grammars.rs`            ← `src/extraction/grammars.ts`
- `src/extraction/generated_detection.rs` ← `src/extraction/generated-detection.ts`
- `tests/grammars_test.rs` — grammar-loading smoke test (every grammar parses a snippet)

`src/extraction/wasm-runtime-flags.ts` is **N/A for native** and was NOT
ported: it exists solely to pass `--liftoff-only` to V8 so web-tree-sitter's
WASM compilation doesn't OOM (issues #293/#298). There is no V8 and no WASM
here. Consequences for other modules:
- `WASM_RUNTIME_FLAGS`, `processHasWasmRuntimeFlags`, `buildRelaunchArgv`,
  `relaunchWithWasmRuntimeFlagsIfNeeded` have no Rust equivalents; the CLI
  port must NOT re-exec itself.
- `CODEGRAPH_WASM_RELAUNCHED` is obsolete. **`CODEGRAPH_HOST_PPID` is NOT
  obsolete** as a concept: the TS MCP server reads it for the #277 PPID
  watchdog. In the native binary there is never an intermediate relauncher
  process, so the watchdog can always use the real parent PID; the MCP/daemon
  port should still honor `CODEGRAPH_HOST_PPID` if set (cheap, keeps env-var
  parity) but never needs to set it.

---

## CONTRACT FOR THE `languages/` PORT (read this first)

### The trait

`crate::extraction::tree_sitter_types::LanguageExtractor` is the Rust shape
of the TS `LanguageExtractor` config-object interface. Required methods are
the TS required properties; every optional TS property/hook is a
**default-implemented** trait method returning the "absent" value — only
override what the TS object literal actually sets.

```rust
pub type SyntaxNode<'tree> = tree_sitter::Node<'tree>; // use this alias, like TS `SyntaxNode`

pub trait LanguageExtractor: Send + Sync {
    // ---- required (the TS required arrays/fields) ----
    fn function_types(&self) -> &[&str];
    fn class_types(&self) -> &[&str];
    fn method_types(&self) -> &[&str];
    fn interface_types(&self) -> &[&str];
    fn struct_types(&self) -> &[&str];
    fn enum_types(&self) -> &[&str];
    fn type_alias_types(&self) -> &[&str];
    fn import_types(&self) -> &[&str];
    fn call_types(&self) -> &[&str];
    fn variable_types(&self) -> &[&str];
    fn name_field(&self) -> &str;
    fn body_field(&self) -> &str;
    fn params_field(&self) -> &str;

    // ---- optional config (defaults shown) ----
    fn enum_member_types(&self) -> &[&str] { &[] }
    fn field_types(&self) -> &[&str] { &[] }
    fn property_types(&self) -> &[&str] { &[] }
    fn return_field(&self) -> Option<&str> { None }
    fn extra_class_node_types(&self) -> &[&str] { &[] }
    fn methods_are_top_level(&self) -> bool { false }
    fn interface_kind(&self) -> NodeKind { NodeKind::Interface }
    fn package_types(&self) -> &[&str] { &[] }

    // ---- optional hooks (None/false = "hook absent" in TS) ----
    fn resolve_name(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> { None }
    fn extract_property_name(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> { None }
    fn get_signature(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> { None }
    fn get_visibility(&self, node: SyntaxNode<'_>, source: &str) -> Option<Visibility> { None }
    fn is_exported(&self, node: SyntaxNode<'_>, source: &str) -> Option<bool> { None }
    fn is_async(&self, node: SyntaxNode<'_>, source: &str) -> Option<bool> { None }
    fn is_static(&self, node: SyntaxNode<'_>, source: &str) -> Option<bool> { None }
    fn is_const(&self, node: SyntaxNode<'_>, source: &str) -> Option<bool> { None }
    fn visit_node(&self, node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool { false }
    fn classify_class_node(&self, node: SyntaxNode<'_>, source: &str) -> ClassLikeKind { ClassLikeKind::Class }
    fn resolve_body<'t>(&self, node: SyntaxNode<'t>, body_field: &str) -> Option<SyntaxNode<'t>> { None }
    fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome { ImportOutcome::NotHandled }
    fn extract_variables<'t>(&self, node: SyntaxNode<'t>, source: &str) -> Vec<VariableInfo<'t>> { Vec::new() }
    fn get_receiver_type(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> { None }
    fn resolve_type_alias_kind(&self, node: SyntaxNode<'_>, source: &str) -> Option<NodeKind> { None }
    fn is_misparsed_function(&self, name: &str, node: SyntaxNode<'_>) -> bool { false }
    fn extract_bare_call(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> { None }
    fn extract_package(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> { None }
}
```

Supporting types in the same module: `ImportInfo { module_name, signature,
handled_refs }` (+ `ImportInfo::new(module, sig)` with `handled_refs: false`),
`ImportOutcome { NotHandled | Declined | Info(ImportInfo) }`,
`VariableInfo<'t>`, `ClassLikeKind { Class | Struct | Enum | Interface |
Trait }`, `NodeExtra` (the `Partial<Node>` extra: `docstring, signature,
visibility, is_exported, is_async, is_static, is_abstract, qualified_name` —
all `Option`, `Default`), and the `ExtractorContext` trait:

```rust
pub trait ExtractorContext {
    fn create_node(&mut self, kind: NodeKind, name: &str, node: SyntaxNode<'_>, extra: NodeExtra) -> Option<Node>;
    fn visit_node(&mut self, node: SyntaxNode<'_>);
    fn visit_function_body(&mut self, body: SyntaxNode<'_>, function_id: &str);
    fn add_unresolved_reference(&mut self, reference: UnresolvedReference);
    fn push_scope(&mut self, node_id: String);
    fn pop_scope(&mut self);
    fn file_path(&self) -> &str;
    fn source(&self) -> &str;
    fn node_stack(&self) -> &[String];
    fn nodes(&self) -> &[Node];
}
```

`TreeSitterExtractor` implements `ExtractorContext`, so a `visit_node` hook
(objc.ts, scala.ts) gets the same callback surface as TS (`ctx.nodes` →
`ctx.nodes()`, `ctx.createNode(...)` → `ctx.create_node(..., NodeExtra::default())`, etc.).

### Trait-vs-TS semantic mappings (IMPORTANT — these encode TS hook-presence)

- **`is_exported`/`is_async`/`is_static`/`is_const`/`get_visibility`/
  `get_signature` returning `None` == "TS hook absent"** → the node field
  stays unset (omitted from JSON). A language that HAS the hook must return
  `Some(bool)`/`Some(value)` — e.g. TS `isExported: (node) => boolean` must
  be `Some(walk_parents())`, never `None`.
- **`extract_import` is tri-state** (`ImportOutcome`) because TS
  distinguishes "no hook" (generic fallback import node allowed) from "hook
  returned null" (fallback suppressed). A language WITH the hook must return
  `Declined` where the TS hook returns `null` — NEVER `NotHandled`.
- `resolve_name` returning `Some("")` is treated as no-result (TS truthiness).
- `classify_class_node` default `ClassLikeKind::Class` == TS `?? 'class'`.
- `resolve_body` returning `None` falls back to
  `child_by_field_name(body_field)` at every call site (TS `?? getChildByField`).
- `get_receiver_type` returning `Some("")` is treated as no-receiver (TS truthiness).
- `interface_kind()` → override to `NodeKind::Trait` (rust.ts, scala.ts) or
  `NodeKind::Protocol` (objc.ts).

### Deviation: extra `source: &str` parameter

TS hooks `getVisibility(node)`, `isAsync(node)`, `isStatic(node)`,
`isConst(node)`, `classifyClassNode(node)` take only the node and use
web-tree-sitter's `node.text`. Native nodes can't produce text without the
source, so these hooks receive `source: &str` too. Where the TS body used
`child.text`, write `get_node_text(child, source)`.

### Helpers available (`crate::extraction::tree_sitter_helpers`)

```rust
pub fn generate_node_id(file_path: &str, kind: NodeKind, name: &str, line: u32) -> String;
pub fn get_node_text<'s>(node: SyntaxNode<'_>, source: &'s str) -> &'s str; // returns &str — call .to_string() when needed
pub fn get_child_by_field<'t>(node: SyntaxNode<'t>, field_name: &str) -> Option<SyntaxNode<'t>>;
pub fn get_preceding_docstring(node: SyntaxNode<'_>, source: &str) -> Option<String>;
```

`generate_node_id` hash input is exactly the TS string
`{filePath}:{kind}:{name}:{line}` (kind = snake_case `NodeKind::as_str()`),
sha256-hex truncated to 32, prefixed `{kind}:` — verified against
TS-computed vectors in unit tests. IDs match across implementations.

### Node-navigation translation table (TS → native tree-sitter 0.26)

- `node.type` → `node.kind()`
- `node.namedChildCount` → `node.named_child_count()` (usize)
- `node.namedChild(i)` → `node.named_child(i as u32)`
- `node.childCount` / `node.child(i)` → `node.child_count()` / `node.child(i as u32)`
- `node.namedChildren.find/filter/some` → collect via cursor; the wrapper
  uses private helpers `named_children(node) -> Vec` / `find_named_child(node, kind)`
  (copy them — they're 5 lines)
- `node.childForFieldName(f)` → `node.child_by_field_name(f)`
- `node.previousNamedSibling` → `node.prev_named_sibling()`
- `node.parent` → `node.parent()`
- `node.startPosition.row/column` → `node.start_position().row/.column` (usize, rows 0-based; stored `start_line` is `row + 1`)
- `node.startIndex/endIndex` → `node.start_byte()/end_byte()` (bytes, not UTF-16 units — identical for ASCII)
- `node.fieldNameForNamedChild(i)` → `node.field_name_for_named_child(i as u32)`
- `node.isNamed` → `node.is_named()`
- `node.text` → `get_node_text(node, source)`

### Worked example (a complete extractor implementation)

A working reference implementation lives in
`src/extraction/tree_sitter_wrapper.rs` `#[cfg(test)] mod tests` —
`TsTestExtractor`, a faithful subset of `languages/typescript.ts` (node-type
lists, `is_exported` parent walk, `is_const` keyword scan, `extract_import`
returning `Info`/`Declined`). The 12 tests around it show expected node
kinds, qualified names, contains edges, and unresolved references — use them
as the executable spec for what the engine does with your config.

Implementations should be stateless unit structs:

```rust
pub struct GoExtractor;
impl LanguageExtractor for GoExtractor {
    fn function_types(&self) -> &[&str] { &["function_declaration"] }
    // ...
    fn get_receiver_type(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        let receiver = get_child_by_field(node, "receiver")?;
        let text = get_node_text(receiver, source);
        RECEIVER_RE.captures(text).and_then(|c| c.get(1)).map(|m| m.as_str().to_string())
    }
}
```

### What `languages/mod.rs` must export (consumed by the orchestrator wiring)

The TS barrel builds `EXTRACTORS: Partial<Record<Language, LanguageExtractor>>`.
The Rust equivalent the wiring expects:

```rust
pub fn extractor_for(language: Language) -> Option<&'static dyn LanguageExtractor>;
```

Mapping (TS `EXTRACTORS`): typescript+tsx → typescript; javascript+jsx →
javascript; c → cExtractor and cpp → cppExtractor (two extractors in one
`c_cpp.rs`); plus python, go, rust, java, csharp, php, ruby, swift, kotlin,
dart, pascal, scala, lua, luau, objc. All other languages → `None`.

---

## Public API surface of the ported files

### `tree_sitter_wrapper.rs`

```rust
pub struct TreeSitterExtractor<'a> { /* private */ }
impl<'a> TreeSitterExtractor<'a> {
    /// language: None ⇒ detect_language(file_path, Some(source)).
    /// extractor: the per-language config (TS did EXTRACTORS[lang] in the
    /// constructor; here the caller passes languages::extractor_for(lang)).
    pub fn new(file_path: impl Into<String>, source: &'a str,
               language: Option<Language>,
               extractor: Option<&'a dyn LanguageExtractor>) -> Self;
    pub fn language(&self) -> Language;
    pub fn extract(mut self) -> ExtractionResult;   // consumes self
}
impl ExtractorContext for TreeSitterExtractor<'_> { ... }
```

Error parity: unsupported language → `ExtractionError{ message:
"Unsupported language: {lang}", code: "unsupported_language" }`; parser
construction failure → `"Failed to get parser for language: {lang}"` /
`"parser_error"`; null tree → `"Parse error: Parser returned null tree"` /
`"parse_error"`. File node: `id = "file:{path}"`, `is_exported:
Some(false)`, `end_line = source.split('\n').count()`.

### `grammars.rs`

```rust
pub const EXTENSION_MAP: &[(&str, Language)];                 // TS insertion order
pub fn language_for_extension(ext: &str) -> Option<Language>; // lowercase, dot-prefixed
pub fn is_source_file(file_path: &str) -> bool;
pub fn is_play_routes_file(file_path: &str) -> bool;
pub fn has_grammar(language: Language) -> bool;               // TS GrammarLanguage
pub fn grammar_language(language: Language) -> Option<tree_sitter::Language>;
pub fn create_parser(language: Language) -> Option<Parser>;   // TS getParser
pub fn detect_language(file_path: &str, source: Option<&str>) -> Language;
pub fn is_language_supported(language: Language) -> bool;
pub fn is_grammar_loaded(language: Language) -> bool;
pub fn is_file_level_only_language(language: Language) -> bool;
pub fn get_supported_languages() -> Vec<Language>;
pub fn get_language_display_name(language: Language) -> &'static str;
// no-op parity shims (native grammars are compiled in):
pub fn init_grammars();
pub fn load_grammars_for_languages(_languages: &[Language]);
pub fn load_all_grammars();
pub fn is_grammars_initialized() -> bool;            // always true
pub fn reset_parser(_language: Language);
pub fn clear_parser_cache();
pub fn get_unavailable_grammar_errors() -> HashMap<Language, String>; // always empty
```

All 21 grammar crates expose modern `LANGUAGE: LanguageFn` consts (verified:
typescript/tsx, javascript, python, go, rust, java, c, cpp, c-sharp,
php→`LANGUAGE_PHP`, ruby, swift, kotlin-ng, **dart 0.2 (modern API — no raw
bridge needed)**, objc, pascal, scala, lua, luau). `tests/grammars_test.rs`
proves each one constructs a parser and cleanly parses a snippet.

### `generated_detection.rs`

```rust
pub fn is_generated_file(file_path: &str) -> bool;
```

### `tree_sitter_types.rs` / `tree_sitter_helpers.rs`

See the contract section above.

---

## Deferred to the orchestrator/wiring task (NOT in my files)

1. **`extractFromSource` (bottom of tree-sitter.ts)** — the dispatcher that
   routes to `IdaCExtractor`/`SvelteExtractor`/`VueExtractor`/
   `LiquidExtractor`/`MyBatisExtractor`/`DfmExtractor`, the
   file-level-only branch, and post-pass framework extractors. Those modules
   were concurrent stubs at port time, so the dispatcher can't live here.
   Wiring recipe (1:1 with TS):
   - ida check: `(lang == C || Cpp) && is_ida_generated_c(path, source)` → IdaCExtractor
   - svelte/vue/liquid/xml → their extractors
   - `is_file_level_only_language(lang)` → empty `ExtractionResult` (zero nodes, duration 0)
   - pascal + (`.dfm`|`.fmx`) → DfmExtractor
   - else → `TreeSitterExtractor::new(path, source, Some(lang), languages::extractor_for(lang)).extract()`
   - then framework `extract` hooks appended (errors as warnings:
     `"Framework extractor '{name}' failed: {err}"`).
2. The TS re-export `export { generateNodeId } from './tree-sitter-helpers'`
   in tree-sitter.ts → `extraction/mod.rs` should re-export
   `tree_sitter_helpers::generate_node_id` (mod.rs is stitched later; I did
   not touch it).
3. `parse-worker.ts` has no equivalent — native parallelism is rayon in the
   orchestrator; `reset_parser`/`load_grammars_for_languages` calls become
   no-ops via the shims.

## Other deviations / Node-isms dropped

- **No parser cache**: TS cached `Parser` per language to manage WASM heap;
  `create_parser` builds a fresh parser per call (cheap natively, and
  `Parser` is not `Sync` so a global cache would need locking under rayon).
- **WASM OOM handling dropped**: the TS re-throw of "memory access out of
  bounds" (to crash the worker for a heap reset) and `tree.delete()` /
  `this.source = ''` have no native equivalent.
- **Columns/byte offsets** are UTF-8 byte-based (web-tree-sitter used UTF-16
  code units). Lines are unaffected. Variable initializer signatures
  truncate at 100 **chars** (TS sliced 100 UTF-16 units).
- `getUnavailableGrammarErrors` is always empty natively — `codegraph status`
  port should still call it for output parity (it'll just never have rows).
- TS quirk preserved on purpose: the anonymous-class `extends` reference uses
  the **0-based** row (TS forgot the `+1` there); everything else is 1-based.
- `extract_variables` hook + `VariableInfo`: declared in the TS interface but
  never called by the TS core; ported for interface parity, still uncalled.
- `detect_language` for a path without `.`: JS `substring(-1)` clamps to the
  whole string (never matches) — reproduced exactly.

## Integrator checklist

- [ ] `extraction/mod.rs` re-exports: `tree_sitter_types::*`,
      `tree_sitter_helpers::{generate_node_id, get_node_text, get_child_by_field, get_preceding_docstring}`,
      `grammars::*` (TS `index.ts` re-exports detectLanguage, isSourceFile,
      isLanguageSupported, isGrammarLoaded, getSupportedLanguages,
      initGrammars, loadGrammarsForLanguages, loadAllGrammars),
      `tree_sitter_wrapper::TreeSitterExtractor`,
      `generated_detection::is_generated_file`.
- [ ] Implement `extract_from_source` dispatcher (recipe above) once
      languages/ + standalone extractors land.
- [ ] `languages/mod.rs` must export
      `extractor_for(Language) -> Option<&'static dyn LanguageExtractor>`.
- [ ] CLI/MCP ports: no `--liftoff-only` relaunch; keep honoring
      `CODEGRAPH_HOST_PPID` if set (see top of this file).

# Language Adapter Parity Matrix

This document tracks what each `LanguageAdapter` in `analysis/src/adapter/`
extracts from source, measured against the Rust adapter (the reference
implementation).

**Legend**
- ✓ — feature implemented
- ✗ — feature absent (graph data is lossy / queries will return nothing)
- ~ — partial / degraded support (see notes column)
- n/a — feature does not exist in this language

## Snapshot — extraction coverage

| Feature                              | Rust | Python | TypeScript | Go  | Java | Kotlin | Swift | C   | C++ | C#  | Ruby | PHP |
|--------------------------------------|:----:|:------:|:----------:|:---:|:----:|:------:|:-----:|:---:|:---:|:---:|:----:|:---:|
| Functions / methods                  |  ✓   |   ✓    |     ✓      |  ✓  |  ✓   |   ✓    |   ✓   |  ✓  |  ✓  |  ✓  |  ✓   |  ✓  |
| Structs / classes                    |  ✓   |   ✓    |     ✓      |  ✓  |  ✓   |   ✓    |   ✓   |  ✓  |  ✓  |  ✓  |  ✓   |  ✓  |
| Enums (as `NodeKind::Enum`)          |  ✓   |   ✗    |     ✓      |  ✗  |  ✓   |   ✓    |   ✓   |  ✓  |  ✓  |  ✓  | n/a  |  ✓  |
| **EnumVariant nodes**                |  ✓   |   ✗    |     ✗      |  ✗  |  ✗   |   ✗    |   ✗   |  ✗  |  ✗  |  ✗  | n/a  |  ✗  |
| **Field nodes** (struct/class field) |  ✓   |   ✗    |     ✗      |  ✗  |  ✗   |   ✗    |   ✗   |  ✗  |  ✗  |  ✗  |  ✗   |  ✗  |
| Traits / Interfaces / Protocols      |  ✓   |  n/a   |     ✓      |  ✓  |  ✓   |   ✓    |   ✓   | n/a |  ✓  |  ✓  | n/a  |  ✓  |
| **Type aliases**                     |  ✓   |   ✗    |     ✗      |  ✗  | n/a  |   ✗    |   ✗   |  ✗  |  ✗  |  ✗  | n/a  |  ✗  |
| **Constants / module-level statics** |  ✓   |   ✗    |     ✗      |  ✗  |  ✗   |   ✗    |   ✗   |  ✗  |  ✗  |  ✗  |  ✗   |  ✗  |
| Modules / packages / namespaces      |  ✓   |   ~¹   |     ✓      |  ✓  |  ✓   |   ✓²   |   ✗   |  ✗  |  ✗  |  ✓  |  ✗   |  ✓  |
| Call edges (`EdgeKind::Calls`)       |  ✓   |   ✓    |     ✓      |  ✓  |  ✓   |   ✓    |   ✓   |  ✓  |  ✓  |  ✓  |  ✓   |  ✓  |
| `UsesType` edges                     |  ✓   |   ✗    |     ✗      |  ✗  |  ✓   |   ✗    |   ✗   |  ✓  |  ✓  |  ✓  |  ✓   |  ✓  |
| `Implements` / inheritance edges     |  ✓   |   ✗    |     ✗      |  ✗  |  ✓   |   ✓    |   ✓   | n/a |  ✓  |  ✓  |  ✗   |  ✓  |
| `UnresolvedCall` edges               |  ✓   |   ✗    |     ✗      |  ✗  |  ✗   |   ~³   |   ~³  |  ✗  |  ✗  |  ✗  |  ✗   |  ✗  |
| `Contains` edges                     |  ✓   |   ✗    |     ✗      |  ✗  |  ✗   |   ✗    |   ✗   |  ✗  |  ✗  |  ✗  |  ✗   |  ✗  |
| Complexity metrics (`complexity.rs`) |  ✓   |   ✓    |     ✓      |  ✓  |  ✓   |   ✓    |   ✓   |  ✓  |  ✓  |  ✓  |  ✓   |  ✓  |
| Control-flow graph (`cfg.rs`)        |  ✓   |   ✗    |     ✗      |  ✗  |  ✗   |   ✗    |   ✗   |  ✗  |  ✗  |  ✗  |  ✗   |  ✗  |
| Dataflow (`dataflow.rs`)             |  ✓   |   ✗    |     ✗      |  ✗  |  ✗   |   ✗    |   ✗   |  ✗  |  ✗  |  ✗  |  ✗   |  ✗  |
| `accessed_fields` metadata           |  ✓   |   ✗    |     ✗      |  ✗  |  ✗   |   ✗    |   ✗   |  ✗  |  ✗  |  ✗  |  ✗   |  ✗  |
| `param_count` metadata               |  ✓   |   ✗    |     ✗      |  ✗  |  ✗   |   ✗    |   ✗   |  ✗  |  ✗  |  ✗  |  ✗   |  ✗  |
| `async` metadata                     |  ✓   |   ✗    |     ✗      |  ✗  | n/a  |   ✗    |   ✗   | n/a |  ✗  |  ✗  | n/a  |  ✗  |
| Visibility detection (≠ Public)      |  ✓   |   ✗⁴   |     ✗      |  ✗  |  ✗   |   ✗    |   ✗   |  ✗  |  ✗  |  ✓  |  ✗   |  ✓  |
| Signature extraction                 |  ✓   |   ✗    |     ✗      |  ✗  |  ✗   |   ✗    |   ✗   |  ✗  |  ✗  |  ✗  |  ✗   |  ✗  |
| Call-site post-pass (`CallSite`)     |  ✓   |   ✗    |     ✗      |  ✗  |  ✗   |   ✗    |   ✗   |  ✗  |  ✗  |  ✗  |  ✗   |  ✗  |
| Lenient parsing (partial trees)      |  ✓   |   ✗    |     ✗      |  ✗  |  ✗   |   ✗    |   ✗   |  ✗  |  ✗  |  ✗  |  ✗   |  ✗  |

¹ Python — module identity is the file itself; no `module` node is emitted.
² Kotlin — `object` declarations are mapped to `NodeKind::Module`; package
  declarations are not surfaced as module nodes.
³ Kotlin / Swift — unresolved callees are synthesised as `NodeKind::Function`
  with an ID derived from the file path. They are not flagged with
  `EdgeKind::UnresolvedCall`, so the resolver pipeline cannot distinguish them
  from real definitions.
⁴ Python — `_`-prefix naming convention is not consulted; everything is
  reported as `Visibility::Public`.

## Notes

- **EnumVariant** and **Field** nodes are critical for renaming and
  refactor-impact analysis (e.g. "every site that pattern-matches `Some`",
  "every read of `self.config.port`"). Rust is the only adapter that emits
  them today.
- Only the Rust adapter populates per-function `cfg`, `dataflow`, and
  `accessed_fields`. Every other adapter leaves the analysis fields `None`,
  so `taint`, `preconditions`, and similar DSL operators degrade to empty
  results on non-Rust files.
- Only C# and PHP attempt real visibility inference; everywhere else nodes
  default to `Visibility::Public`, which makes the `pub`-aware queries (e.g.
  `entrypoints kind=PublicApi`) misleading on those languages.
- No adapter except Rust overrides `extract_call_sites`, so the cross-file
  reference resolver is effectively Rust-only.

## Highest-value gaps

The next three adapters by language popularity are TypeScript, Python, and Go.
The follow-up work in this commit closes the most painful gaps for them:

| Adapter    | Adds EnumVariant | Adds Field | Adds TypeAlias | Adds Constant |
|------------|:----------------:|:----------:|:--------------:|:-------------:|
| TypeScript |        ✓         |     ✓      |       ✓        |       ✓       |
| Python     |       n/a (no enum syntax surfaced)        |     ✓      |      n/a       |       ✓       |
| Go         |        n/a (Go has no enums)               |     ✓      |       ✓        |       ✓       |

Java/Kotlin/Swift/C#/PHP/C/C++ remain unchanged in this pass — they are still
missing field, variant, alias and constant nodes, and should be addressed in a
later milestone.

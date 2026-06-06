# frameworks group 2 — backend ecosystems port notes

Ported from `src/resolution/frameworks/{python,laravel,drupal,ruby,java,play,csharp}.ts`
into `rust/src/resolution/frameworks/{python,laravel,drupal,ruby,java,play,csharp}.rs`.

Tests: `rust/tests/frameworks_backend_test.rs` — 64 tests, all green
(`cargo test --test frameworks_backend_test`). `cargo check` clean for these
seven files (zero warnings).

## Public API (what frameworks/mod.rs / the registry should wire)

Every TS `export const fooResolver: FrameworkResolver = {...}` becomes a
**stateless unit struct** implementing
`crate::resolution::types::FrameworkResolver`:

| TS export | Rust type | `name()` | `languages()` |
|---|---|---|---|
| `djangoResolver` | `python::DjangoResolver` | `"django"` | `[Python]` |
| `flaskResolver` | `python::FlaskResolver` | `"flask"` | `[Python]` |
| `fastapiResolver` | `python::FastapiResolver` | `"fastapi"` | `[Python]` |
| `laravelResolver` | `laravel::LaravelResolver` | `"laravel"` | `[Php]` |
| `drupalResolver` | `drupal::DrupalResolver` | `"drupal"` | `[Php, Yaml]` |
| `railsResolver` | `ruby::RailsResolver` | `"rails"` | `[Ruby]` |
| `springResolver` | `java::SpringResolver` | `"spring"` | `[Java, Kotlin, Yaml, Properties]` |
| `playResolver` | `play::PlayResolver` | `"play"` | `[Scala, Java, Yaml]` |
| `aspnetResolver` | `csharp::AspnetResolver` | `"aspnet"` | `[Csharp]` |

Registry construction: `Box::new(DjangoResolver) as Box<dyn FrameworkResolver>`
etc. — all are zero-sized, no constructors.

Also exported: `laravel::FACADE_MAPPINGS: &[(&str, &str)]` (TS
`Record<string,string>` → static slice of pairs; exported-for-future-use as in TS).

All implement `detect`, `resolve`, `extract` (always `Some(...)` — the TS
objects all define the hook) and, where the TS object had it,
`claims_reference` (django `_iterable_class`, laravel `Controller@method`,
drupal `hook_*`/FQCN/`Class::?method`, rails `controller#action`, spring
`*:prefix`, play `Class.method`). None implement `post_extract` (matches TS —
`None` = hook absent).

## Route node shapes / metadata keys — kept byte-identical

- Node `id`/`name`/`qualified_name` format strings are verbatim from TS
  (e.g. `route:{file}:{line}:{METHOD}:{path}`, `VIEWSET /{prefix}`,
  `{file}::route:{path}`, drupal `{file}::{routeName}`, spring config
  `spring-config:{file}:{line}:{dotted}`, `spring-value:…`, `spring-cp:…`,
  `{file}::@Value:{key}`, `{file}::@ConfigurationProperties:{prefix}`).
- Spring `@ConfigurationProperties` reference names keep the `:prefix`
  sentinel suffix.
- Confidence values, edge kinds (`references` vs `imports` for django
  `include()` / laravel resource controllers), and per-pattern directory
  preference lists are unchanged.

## Mapping decisions / faithful-port notes

- **Line numbers**: TS `content.slice(0, m.index).split('\n').length` →
  `content[..m.start()].matches('\n').count() + 1`. Byte offsets vs UTF-16
  offsets are equivalent here because only `\n` bytes are counted and
  `strip_comments_for_regex` preserves byte length.
- **`end_column`** route fields use the regex match's **byte** length
  (`m.as_str().len()`); TS used UTF-16 `match[0].length`. Identical for
  ASCII source (always, in these grammars' route syntax).
- **600-char lookahead** (spring/aspnet handler search) is 600 **bytes**
  clamped to a char boundary (TS sliced 600 UTF-16 units) — `slice_bounded`.
- **Regexes** are `regex` crate ports, compiled once via
  `std::sync::LazyLock`. `\w`/`\b` are Unicode in Rust (ASCII in JS) — a
  superset; identical on ASCII identifiers. No lookarounds/backreferences
  were needed. JS `[[(]` became `[\[(]` (regex crate requires the escape).
  Drupal's `[^'"#\n]` classes forced `r##"…"##` raw strings.
- **Drupal hook map**: TS insertion-ordered `Map` → `HashMap` (lookup) +
  `Vec` (Strategy-B iteration order), first-declaration-wins preserved.
- **Drupal node IDs** for hook implementers reuse
  `crate::extraction::tree_sitter_helpers::generate_node_id` (same sha256
  scheme as TS `generateNodeId` — IDs match across implementations).
- **composer.json parsing** uses `serde_json::Value`; malformed JSON falls
  through to the `*.info.yml` fallback exactly like the TS try/catch.
- **rails_snake_case** reproduces the TS quirk
  `replace(/([A-Z])/g,'_$1').toLowerCase().slice(1)` including the
  unconditional first-char drop.
- **Spring YAML/properties mini-parsers** ported line-for-line (escaped-sep
  scan in .properties, quote-aware colon scan + indent stack in YAML,
  `strip_wrapping_quotes` = TS `replace(/^["']|["']$/g,'')`). One
  Rust-safety guard added: a `:` *inside* the indent region (impossible in
  practice — indent is whitespace) is skipped where JS `slice` would have
  produced an empty key and skipped anyway; observable behavior unchanged.
  `docstring` truncation is 200 **chars** (TS: 200 UTF-16 units).
- Spring resolve's TS dead code (an empty `if` block at java.ts:82–86, comment
  only) was dropped — it had no effect.
- `Date.now()` → private `now_millis()` per file (epoch ms as `i64`).
  PORTING.md forbids touching `utils.rs`, hence the 3-line duplication.
- Play's `extract` consumes `crate::extraction::grammars::is_play_routes_file`
  (already ported); language of Play route nodes stays `scala`.

## Tests ported vs deferred

Ported into `tests/frameworks_backend_test.rs` (64 tests):
- `frameworks.test.ts`: all django (6) / flask (6) / fastapi (4) /
  laravel (3) / rails (2) / spring (3) / play (2 + routes-file detection 1) /
  aspnet (1) extract cases, plus the commented-out-route regressions for
  django, flask, fastapi, laravel, rails, spring, aspnet (7).
- `drupal.test.ts`: detect (8), claimsReference (2), routing.yml extract (9),
  hook detection (6), resolve (4). The fixture `ResolutionContext`
  (`FixtureContext`) mirrors the TS `makeContext` overrides by filtering one
  `Vec<Node>`.

DEFERRED to the stitch/pipeline agent (need `CodeGraph` + `indexAll`):
- `frameworks.test.ts` → `getApplicableFrameworks` suite (lives in
  frameworks/index.ts → `frameworks/mod.rs`, owned by the registry owner) and
  the generic "FrameworkResolver.extract interface" smoke (covered by the
  resolution-types contract tests).
- `frameworks-integration.test.ts` → "Django end-to-end framework
  extraction", "Flask end-to-end" (stacked routes across @login_required),
  and the whole "Java end-to-end" describe: field-injected bean trace
  (#389), MyBatis mapper bridging, **@Value/@ConfigurationProperties YAML +
  .properties relaxed-binding end-to-end**, non-MyBatis XML file-node, and
  `this.field.method()` unique-impl resolution. (Flutter/C++/Go-gRPC/JVM-FQN
  suites in that file belong to other waves.)
- `drupal.test.ts` → "Drupal end-to-end — route node linked to controller
  method".

## Integration needs

- `frameworks/mod.rs` (NOT mine) still only declares `pub mod` lines. The
  registry owner must port `frameworks/index.ts`: `FRAMEWORK_RESOLVERS`
  ordering (TS order: express, nestjs, fastify?, … — check index.ts), and
  `getApplicableFrameworks(resolvers, language)` filtering by
  `languages()` containing the language, `None` = always applicable.
- The orchestrator's framework-extract hook should call
  `resolver.extract(file_path, content)` and treat `Some(result)` as the TS
  return value (these seven always return `Some`).
- Spring config extraction depends on the orchestrator routing
  `application*.yml|properties` files through framework `extract` hooks even
  though their language (`yaml`/`properties`) is file-level-only — TS does
  this via the framework post-pass in tree-sitter.ts `extractFromSource`.

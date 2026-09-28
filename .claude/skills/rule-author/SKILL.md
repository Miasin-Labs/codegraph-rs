---
name: rule-author
description: Write codegraph bug rules (YAML, tree-sitter queries + index predicates) that find a bug's variants, and keep only the ones that beat chance. Use when a bug was just found or fixed and its siblings may exist elsewhere, when asked to write/check/score/save a rule for `codegraph analyze rules`, or when turning a bug class (a CWE, a misuse pattern) into a detector. Drives the `codegraph_rules` MCP tool (opt-in: CODEGRAPH_MCP_TOOLS=rules,...) or the `codegraph analyze rules` CLI.
---

# Writing bug rules that earn their place

A rule is YAML the engine runs over the index: a syntax pattern (a
tree-sitter `query`, or a weggli `pattern` for C/C++) plus `where`
predicates the syntax cannot answer (what a call *resolves to* in the index,
what the enclosing function calls, which ancestor a match sits in). Every
rule carries `examples: {bad, good}`. A rule is worth keeping only if, on
labeled code, its precision beats the **base rate** — the precision of
firing at random — by a margin, on enough findings to trust it.

The tool: `codegraph_rules` with `action` = `variant` | `check` | `run` |
`score` | `save`. CLI equivalents: `codegraph analyze rules --check FILE`,
`codegraph analyze rules FILE`, `codegraph analyze rules FILE --score
CORPUS`; `save` has no CLI — write `.codegraph/rules/<id>.yaml` yourself
after `--check` passes. Saved rules run on every `analyze rules` sweep
(`--no-saved` / `saved: false` leaves them out; a rule you pass with the
same id shadows the saved one, so you can edit a saved rule in place).

## Loop A — from a bug to a rule for its variants

1. **`variant`** with `at: "<file>:<line>"` (a finding's place, or the line
   you fixed). You get the enclosing function (numbered), the statement at
   the line as a compact s-expression **with field names** (your query
   vocabulary — copy node kinds and fields from it, never guess them), the
   calls in it with what the index resolves each to, any detector finding
   there with its evidence, and a **skeleton rule** that already passes
   `check`: a query for a resolved call, `resolves-to` pinned to its
   qualified name, `inside` the statement's kind, and the statement as its
   `bad` example (with the `resolves` map `check` needs to resolve calls
   without an index).
2. **Write the rule: generalize the skeleton.** Ask what makes this a bug
   *anywhere*, not here: replace the pinned name with the property that
   matters (`^\w+::\w+$` = any project method; `^reqwest::` = a crate),
   express the context as `inside`/`not-inside` ancestors, move the fix's
   shape into a `good` example. Write the `description` as the bug class
   and the `message` with `{capture}`/`{function}`.
3. **`check`** until every example passes. A failure says exactly what
   happened: `good[1] matched check-patterns[0] at example line 4:
   \`Some(1)\`` or `bad[0] did not match; … matched at example line 7 but
   where[1] (inside …) rejected it: no enclosing node matches`. Add the
   near-miss that fooled it as a new `good` example before fixing.
4. **`run`** on the project (`saved: false` while iterating). Zero findings
   on the code the bug came from means the rule is narrower than your
   example — the real code differs (braces, comments, a wrapper call);
   read the `tree` again. Many findings means it is noisy: read a few,
   tighten, add the false positives as `good` examples, re-check.
5. **`score`** on a bugbench corpus with this bug class, if one exists
   (below). `keep` → go on; `discard` → read the reason, tighten or drop.
   Project-specific logic bugs usually have no corpus: then the evidence is
   step 4 (it fires on the buggy version, not the fixed one).
6. **`save`** (refused unless `check` passes). It now runs on every sweep.

## Loop B — from a bug class

Write the rule (start from a real bad/good pair — for Juliet, a testcase's
`bad()` and `good1()`), `check`, then `score` on the matching corpus and
keep or discard. Don't tune a rule to one corpus's quirks (Juliet is
synthetic and very regular; it over-fits easily) — prefer a second corpus
or real code for confirmation.

## Scoring

`score` (MCP: `corpus`, `sample`, `seed`, `wait`, `cursor`; CLI: `--score
DIR --sample N --work DIR --deadline S --cursor C`) stages each unit of the
corpus, indexes it exactly as `tools/bugbench` does (scratch
`CODEGRAPH_HOME`, atlas/deps/external off), runs the rules, and scores with
bugbench's semantics — the numbers are identical to `score.py` on the same
sample:

- **labeled** corpora (Juliet, OWASP, web apps): a finding inside a `bad`
  row is TP, inside only a `good` row FP, elsewhere unlabeled (not counted).
  Base rate = bad ÷ (bad + good) rows of the scored files.
- **pairs** (`rustsec-adjacent`): a finding near the fix in `vuln/` that is
  gone from the same function in `fixed/` is TP; gone but off the fix is
  `offFix`; still there after the fix is `notDiscriminating`/`background`.
  Base rate = share of the functions the fix changed that lie in its fix
  region (where a finding that vanishes with the fix lands by chance).
- **verdict** `keep` needs ≥ 5 scored findings, precision ≥ base + 10 pts,
  and a 95% one-sided Wilson lower bound above the base rate.

Indexed units and per-rule findings are cached under the work dir, so
re-scoring an edited rule costs no re-index. A run cut by `wait`/
`--deadline` returns `complete: false` and a `nextCursor`; call again with
it until `complete`. Verdicts of an incomplete run are provisional.

Corpora (bugbench, `$BUGBENCH_ROOT`, see its MANIFEST.md): `juliet-c`
(C/C++ CWEs; `sample` = testcases per CWE, default 40), `juliet-java`,
`owasp-benchmark-java` (injection/crypto; built to punish pattern-only
rules), `webapps/<app>` (positives only: recall, not precision),
`rustsec-adjacent` (Rust soundness/logic at the fix boundary; `sample` =
pairs).

## Rule-writing notes

- Query syntax: fields `name: (kind)`, anchors `.` (first/last named child
  or adjacency), alternations `[ … ]`, quantifiers `?*+`, text predicates
  `#eq?`/`#match?`/`#any-of?`. Comments are named nodes: `(block (x) .)`
  fails if a comment follows `x`.
- `where` predicates: `resolves-to`/`not-resolves-to` (capture's call →
  qualified callee; dependency calls also as `<crate>::<qname>`),
  `inside`/`not-inside` (a query matched *at* some ancestor),
  `enclosing-function {calls, calls-not, name-regex, is-test}`, capture
  `regex`/`not-regex`, and `reached-from: [route, extractor, listener,
  message, public-api]` (`server` = the first four): the match's function
  is reached, within 8 resolved calls, from such an entry point (evidence:
  the path); `via-type: true` also counts a method whose type's other
  methods are (a constructor), `unreached-confidence: 0.3` keeps unreached
  matches ranked lower instead of dropping them. Unresolved means not
  reached. In examples, calls resolve as written unless the example's
  `resolves: {self.send: Client::send}` maps them, and code is reached
  only if it says `reached: true` (or the kinds: `reached: [listener]`).
- Gate a rule on `reached-from: server` when its bug needs untrusted input
  a server takes (redirects, waits with no deadline); measure first which
  of its findings the gate drops, and prefer `unreached-confidence` when
  some outside a server are real.
- A `call_expression` is a call in C/Rust/JS/Go; Java `method_invocation`,
  Python `call`, PHP `function_call_expression`/`member_call_expression`.
- Report where the reader should look: `at: <capture>`.

## Worked examples

### 1. rms KeepBoth (from a bug; `rms-91a8e8b`, `src/integrations/sync.rs:1922`)

`analyze bugs` flagged `arm-result-deviance`: the `KeepBoth` arm downloads a
conflict copy then yields `None`, while `UseLocal`/`UseCloud` yield their
`upload_counted`/`download_counted` result — so nothing is recorded and every
sync makes the copy again. `variant` at `sync.rs:1922` gave the `match_arm`
tree, `self.download_file → CloudSync::download_file`, and a skeleton pinned
to that call `inside (match_arm)`. Generalized:

```yaml
id: arm-work-yields-none
language: rust
severity: high
message: "`{call}` runs in an arm that yields `None` while a sibling arm yields its call's result: what this arm did is not reported to the caller of {function}"
check-patterns:
  - name: self-call-in-none-arm
    query: |
      (call_expression
        function: (field_expression value: (self) field: (field_identifier) @method)) @call
    where:
      - capture: call
        resolves-to: '^\w+::\w+$'          # any project method
      - capture: call
        inside: |
          ((match_arm value: (block (identifier) @tail .)) (#eq? @tail "None"))
      - capture: call
        inside: |
          (match_block
            (match_arm value: [
              (await_expression (call_expression function: (field_expression value: (self))))
              (call_expression function: (field_expression value: (self)))
              (block (await_expression (call_expression function: (field_expression value: (self)))) .)
              (block (call_expression function: (field_expression value: (self))) .)]))
examples: …   # bad: the KeepBoth shape; good: the fix (arm yields upload_counted),
              # a None arm doing no project work, and `Choice::B => Some(1)`
```

How the loop got there: the first version's sibling test was
`(match_arm value: (await_expression …))` — `check` passed but `run` found
**nothing**: the real arms are braced (`=> { self.upload_counted(…).await }`),
so the value is a `block`. Adding `(block … .)` alternatives made `check`
fail on a `good` example (`Some(1)` is a `call_expression` too), so the
sibling call was pinned to a `self` method. Final: `run` → exactly one
finding, `sync.rs:1938` in `CloudSync::sync_both` on `rms-91a8e8b`, none on
the fixed `rms-7ab4824` (whose arm ends in `self.upload_counted(…).await`).
`score` on `rustsec-adjacent` → `discard: no findings` (that corpus has no
such bug; the evidence is the version pair), then `save`.

### 2. C switch fall-through (from a bug class; Juliet CWE-484)

```yaml
id: c-case-falls-through
language: [c, cpp]
message: "this case falls through into the next one after `{last}`"
check-patterns:
  - query: |
      (compound_statement
        (case_statement (expression_statement) @last .) @case
        .
        (case_statement))
    at: case
examples:
  bad:  [a case with printLine("0"); then case 1:]
  good: [every case ends in break; grouped `case 1: case 2:`; a `/* fall through */` comment]
```

`score` on `juliet-c --sample 10`: 10 findings, 10 TP, 0 FP, base rate
28.5% → **keep** (precision 100%, lower bound 78.7%; recall 3.4% — it only
targets one CWE of 22).

### 3. A naive CWE-78 rule (discarded)

`((call_expression function: (identifier) @f) @call (#match? @f
"^(system|popen|execl|…)$"))` on `juliet-c --sample 10`: 4 TP, 6 FP
(the `good` variants run the same sink on a constant) → precision 40.0% vs
base 28.5%, lower bound 19.4% → **discard**. The sink alone does not
separate bad from good; the rule needs the input source
(`enclosing-function: {calls: "fgets|recv|getenv"}`) or a taint analysis —
or it is not worth keeping.

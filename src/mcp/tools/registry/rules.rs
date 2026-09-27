//! Schema for the bug-rule authoring tool (opt-in via CODEGRAPH_MCP_TOOLS).

use serde_json::{Map, Value};

use super::super::rules::rules_output_schema;
use super::super::schema::{InputSchema, ToolAnnotations, ToolDefinition};
use super::schema_builder::{project_path_property, prop, prop_default, prop_enum};

/// `save` writes the project's rules directory and `score` stages and
/// indexes corpus units, so unlike the graph tools it is not read-only;
/// nothing it writes is destructive, and it never reaches the network.
fn rules_annotations() -> Option<ToolAnnotations> {
    Some(ToolAnnotations {
        read_only_hint: Some(false),
        destructive_hint: Some(false),
        idempotent_hint: Some(true),
        open_world_hint: Some(false),
    })
}

pub(in crate::mcp::tools::registry) fn push_rules_tool(out: &mut Vec<ToolDefinition>) {
    let mut props = Map::new();
    props.insert(
        "action".into(),
        prop_enum(
            "string",
            "`variant`: from a bug at `at` (file:line) → its function, syntax tree, resolved \
             calls and a skeleton rule to generalize. `check`: run `yaml`'s examples, no index \
             — every bad example must match, no good one — and say why one fails. `run`: the \
             rules over this project → findings. `score`: the rules on a labeled `corpus` → \
             precision, recall, base rate, keep/discard. `save`: a rule that passes check → \
             .codegraph/rules/<id>.yaml, run by every later sweep.",
            &["check", "run", "variant", "save", "score"],
        ),
    );
    props.insert(
        "yaml".into(),
        prop(
            "string",
            "Rule YAML (one rule, a list, or `---` documents): id, severity, language, \
             check-patterns (a tree-sitter `query`, or a weggli `pattern` for C/C++, with `where` \
             predicates resolves-to / not-resolves-to / regex / not-regex / inside / not-inside \
             / enclosing-function {calls, calls-not, name-regex, is-test}), ignore-patterns, \
             message ({capture}, {function}) and examples {bad, good}; an example may map \
             `resolves: {callee-as-written: qualified-name}`. Or `taint` instead of \
             check-patterns: sources/sanitizers (`value` capture), sinks (`argument`), \
             propagators (`from`, `to`), guards (`value`, `check`, `safe`) — flows are followed \
             across calls and files, every step as evidence.",
        ),
    );
    props.insert(
        "builtin".into(),
        prop_default(
            "boolean",
            "Also the built-in rules (check, run, score)",
            Value::from(false),
        ),
    );
    props.insert(
        "saved".into(),
        prop_default(
            "boolean",
            "Also the project's saved rules, each shadowed by a `yaml` rule of its id (run, \
             score; check uses them when no `yaml` is given)",
            Value::from(true),
        ),
    );
    props.insert(
        "at".into(),
        prop(
            "string",
            "variant: the bug's `file:line` (project-relative), e.g. a finding's place",
        ),
    );
    props.insert(
        "tests".into(),
        prop_default(
            "boolean",
            "run: include findings in test code",
            Value::from(false),
        ),
    );
    props.insert(
        "limit".into(),
        prop_default(
            "number",
            "run: findings listed (default: 50)",
            Value::from(50),
        ),
    );
    props.insert(
        "corpus".into(),
        prop(
            "string",
            "score: a tools/bugbench corpus directory holding ground_truth.jsonl (juliet-c, \
             juliet-java, owasp-benchmark-java, webapps/<app>, rustsec-adjacent)",
        ),
    );
    props.insert(
        "sample".into(),
        prop(
            "number",
            "score: Juliet testcases per CWE (default 40), or vulnerable/fixed pairs (default all)",
        ),
    );
    props.insert(
        "seed".into(),
        prop_default("number", "score: sampling seed", Value::from(1)),
    );
    props.insert(
        "cursor".into(),
        prop(
            "string",
            "score: the `nextCursor` of a run that ran out of time, to resume it",
        ),
    );
    props.insert(
        "wait".into(),
        prop_default(
            "number",
            "score: seconds to work before returning (default: 25, max 55). Finished units are \
             cached; call again with `nextCursor` for the rest.",
            Value::from(25),
        ),
    );
    props.insert(
        "work".into(),
        prop(
            "string",
            "score: where corpus units are staged and indexed (default: rulescore-work beside \
             the corpus); indexes there are reused while fresh",
        ),
    );
    props.insert("projectPath".into(), project_path_property());
    out.push(ToolDefinition {
        name: "codegraph_rules".into(),
        description: "Write bug rules that find a bug's variants, and keep only those that beat \
            chance. From a bug: `variant` at its file:line, write the rule from the skeleton, \
            `check` until it passes, `run` it on the project (tighten it if noisy), `score` it on \
            a labeled corpus, `save` it. A saved rule runs on every later sweep."
            .into(),
        input_schema: InputSchema {
            schema_type: "object".into(),
            properties: props,
            required: Some(vec!["action".into()]),
        },
        output_schema: Some(rules_output_schema()),
        annotations: rules_annotations(),
    });
}

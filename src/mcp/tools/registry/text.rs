//! Schema of the text-search tool.

use serde_json::{Map, Value};

use super::super::schema::{InputSchema, ToolDefinition};
use super::super::text::grep_output_schema;
use super::schema_builder::{
    project_path_property,
    prop,
    prop_default,
    prop_enum,
    read_only_annotations,
};

pub(in crate::mcp::tools::registry) fn push_grep_tool(out: &mut Vec<ToolDefinition>) {
    let mut props = Map::new();
    props.insert(
        "pattern".into(),
        prop(
            "string",
            "Regex in Rust/ripgrep syntax, matched per line: `a|b`, `\\bword\\b`, `foo\\(`, \
             `^fn `, `TODO|FIXME`. grep's `a\\|b` is read as alternation too (unless `literal`).",
        ),
    );
    props.insert(
        "literal".into(),
        prop_default(
            "boolean",
            "Match `pattern` as a fixed string, not a regex (grep -F; default: false)",
            Value::Bool(false),
        ),
    );
    props.insert(
        "caseInsensitive".into(),
        prop_default(
            "boolean",
            "Ignore case (grep -i; default: false)",
            Value::Bool(false),
        ),
    );
    props.insert(
        "word".into(),
        prop_default(
            "boolean",
            "Whole words only (grep -w; default: false)",
            Value::Bool(false),
        ),
    );
    props.insert(
        "path".into(),
        prop(
            "string",
            "Only this file, or files under this directory (project-relative; a glob here \
             works as `glob`)",
        ),
    );
    props.insert(
        "glob".into(),
        prop(
            "string",
            "Only files matching this glob (grep --include / rg -g): `*.rs`, `*.{c,h}` match \
             the file name, `src/**/*.ts` the path; a leading `!` excludes",
        ),
    );
    props.insert(
        "mode".into(),
        prop_enum(
            "string",
            "`lines` (default): numbered hit lines; `count`: matching lines per file (grep -c); \
             `files`: file names only (grep -l)",
            &["lines", "count", "files"],
        ),
    );
    props.insert(
        "before".into(),
        prop(
            "number",
            "Context lines before each shown hit (grep -B, max 50)",
        ),
    );
    props.insert(
        "after".into(),
        prop(
            "number",
            "Context lines after each shown hit (grep -A, max 50). With context, a hit whose \
             enclosing definition is at most twice the window shows the whole definition.",
        ),
    );
    props.insert(
        "context".into(),
        prop(
            "number",
            "Context lines on both sides (grep -C); `before`/`after` override it",
        ),
    );
    props.insert(
        "maxPerFile".into(),
        prop_default(
            "number",
            "Hit lines shown per file; the rest are counted in `more` (default: 3)",
            Value::from(3),
        ),
    );
    props.insert(
        "limit".into(),
        prop_default(
            "number",
            "Hit lines shown in total; further files are listed with counts only (default: 30)",
            Value::from(30),
        ),
    );
    props.insert(
        "cursor".into(),
        prop(
            "string",
            "Next page: the `nextCursor` of a previous call, passed with the same pattern, \
             literal, caseInsensitive, word, path, and glob.",
        ),
    );
    props.insert("projectPath".into(), project_path_property());
    out.push(ToolDefinition {
        name: "codegraph_grep".into(),
        description: "Search the text of the project's indexed files — log and error messages, \
            string literals, partial names, regexes, TODO markers — instead of shell grep/rg, \
            scoped to a file, a directory, or a glob. Returns matches grouped per file, code \
            before tests and docs: `N: text` lines under the symbol they sit in (optional \
            before/after context as `N- text`), the file's `count`, and `more` for the lines \
            left out; past 40 files a per-directory summary. Hits already sent this session are \
            listed by line, not repeated. Bounded: a huge search stops after a few seconds and \
            says what it left out, with `nextCursor` to continue. For symbol names use \
            codegraph_search."
            .into(),
        input_schema: InputSchema {
            schema_type: "object".into(),
            properties: props,
            required: Some(vec!["pattern".into()]),
        },
        output_schema: Some(grep_output_schema()),
        annotations: read_only_annotations(),
    });
}

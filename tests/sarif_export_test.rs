//! SARIF 2.1.0 export of codegraph findings: a golden log, validation
//! against the OASIS schema (`tests/sarif/sarif-schema-2.1.0.json`, the
//! errata01 copy), fingerprints that survive moved lines, and a round trip
//! through the SARIF reader.
//!
//! Rewrite the golden file with `CODEGRAPH_UPDATE_GOLDEN=1 cargo test --test
//! sarif_export_test`.

use std::collections::BTreeMap;
use std::path::Path;

use codegraph::analyze::bugs::{Detector, Evidence, Finding};
use codegraph::analyze::sarif::read;
use codegraph::analyze::sarif::write::{RuleDoc, ToolInfo, to_sarif};
use serde_json::Value;

const SCHEMA: &str = include_str!("sarif/sarif-schema-2.1.0.json");
const GOLDEN: &str = "tests/sarif/export.golden.sarif";

// ---------------------------------------------------------------------------
// A validator for the JSON Schema draft-04 keywords the SARIF schema uses:
// $ref (local), type, enum, properties, additionalProperties, required,
// items, minItems, uniqueItems, minimum, pattern, anyOf, format (uri,
// uri-reference, date-time: shape only). Anything else in a schema is
// annotation (description, default, title).
// ---------------------------------------------------------------------------

fn resolve<'a>(root: &'a Value, reference: &str) -> &'a Value {
    let pointer = reference.strip_prefix('#').expect("local $ref");
    root.pointer(pointer)
        .unwrap_or_else(|| panic!("unresolved $ref {reference}"))
}

fn type_matches(value: &Value, name: &str) -> bool {
    match name {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        other => panic!("unknown type {other}"),
    }
}

fn validate(value: &Value, schema: &Value, root: &Value, path: &str, errors: &mut Vec<String>) {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        return validate(value, resolve(root, reference), root, path, errors);
    }
    if let Some(types) = schema.get("type") {
        let ok = match types {
            Value::String(name) => type_matches(value, name),
            Value::Array(names) => names
                .iter()
                .any(|n| type_matches(value, n.as_str().unwrap())),
            _ => true,
        };
        if !ok {
            errors.push(format!("{path}: expected type {types}, got {value}"));
            return;
        }
    }
    if let Some(options) = schema.get("enum").and_then(Value::as_array) {
        if !options.contains(value) {
            errors.push(format!("{path}: {value} not in {options:?}"));
        }
    }
    if let Some(any) = schema.get("anyOf").and_then(Value::as_array) {
        let fits = any.iter().any(|option| {
            let mut sub = Vec::new();
            validate(value, option, root, path, &mut sub);
            sub.is_empty()
        });
        if !fits {
            errors.push(format!("{path}: matches no anyOf branch"));
        }
    }
    if let Some(min) = schema.get("minimum").and_then(Value::as_f64) {
        if value.as_f64().is_some_and(|n| n < min) {
            errors.push(format!("{path}: {value} < minimum {min}"));
        }
    }
    if let (Some(pattern), Some(text)) = (
        schema.get("pattern").and_then(Value::as_str),
        value.as_str(),
    ) {
        if !regex::Regex::new(pattern).unwrap().is_match(text) {
            errors.push(format!("{path}: {text:?} does not match /{pattern}/"));
        }
    }
    if let (Some(format), Some(text)) =
        (schema.get("format").and_then(Value::as_str), value.as_str())
    {
        let ok = match format {
            "uri" => text.contains(':') && !text.contains(char::is_whitespace),
            "uri-reference" => !text.contains(char::is_whitespace),
            "date-time" => text.len() >= 20 && text.as_bytes()[10] == b'T',
            _ => true,
        };
        if !ok {
            errors.push(format!("{path}: {text:?} is not a {format}"));
        }
    }
    if let Some(object) = value.as_object() {
        let properties = schema.get("properties").and_then(Value::as_object);
        for key in schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !object.contains_key(key) {
                errors.push(format!("{path}: missing required `{key}`"));
            }
        }
        for (key, item) in object {
            let child = format!("{path}.{key}");
            match properties.and_then(|p| p.get(key)) {
                Some(sub) => validate(item, sub, root, &child, errors),
                None => match schema.get("additionalProperties") {
                    Some(Value::Bool(false)) => {
                        errors.push(format!("{path}: property `{key}` is not allowed"));
                    }
                    Some(sub @ Value::Object(_)) => validate(item, sub, root, &child, errors),
                    _ => {}
                },
            }
        }
    }
    if let Some(items) = value.as_array() {
        if let Some(min) = schema.get("minItems").and_then(Value::as_u64) {
            if (items.len() as u64) < min {
                errors.push(format!("{path}: {} items < minItems {min}", items.len()));
            }
        }
        if schema.get("uniqueItems") == Some(&Value::Bool(true)) {
            for (i, a) in items.iter().enumerate() {
                if items[..i].contains(a) {
                    errors.push(format!("{path}[{i}]: duplicate item"));
                }
            }
        }
        if let Some(item_schema) = schema.get("items") {
            for (i, item) in items.iter().enumerate() {
                validate(item, item_schema, root, &format!("{path}[{i}]"), errors);
            }
        }
    }
}

fn schema_errors(log: &Value) -> Vec<String> {
    let schema: Value = serde_json::from_str(SCHEMA).unwrap();
    let mut errors = Vec::new();
    validate(log, &schema, &schema, "$", &mut errors);
    errors
}

#[test]
fn the_validator_rejects_what_the_schema_forbids() {
    let bad = serde_json::json!({
        "version": "2.0.0",
        "runs": [{"tool": {"driver": {}}, "results": [{"message": {}, "level": "fatal"}]}]
    });
    let errors = schema_errors(&bad);
    assert!(errors.iter().any(|e| e.contains("$.version")), "{errors:?}");
    assert!(
        errors.iter().any(|e| e.contains("missing required `name`")),
        "{errors:?}"
    );
    assert!(errors.iter().any(|e| e.contains("\"fatal\"")), "{errors:?}");
}

// ---------------------------------------------------------------------------

const SOURCE: &str = "class A {\n\
    void doPost(HttpServletRequest request) {\n\
        String name = request.getParameter(\"name\");\n\
        String sql = \"SELECT * FROM t WHERE n = '\" + name + \"'\";\n\
        statement.executeQuery(sql);\n\
        try { run(); } catch (Exception e) { }\n\
    }\n\
}\n";

fn findings() -> Vec<Finding> {
    let file = "src/main/java/A.java".to_string();
    vec![
        Finding {
            detector: Detector::Rule,
            rule: "java-sqli-taint".into(),
            file: file.clone(),
            line: 5,
            col: 8,
            function: Some("A::doPost".into()),
            message: "request parameter reaches SQL text".into(),
            confidence: 0.9,
            evidence: vec![
                Evidence {
                    file: file.clone(),
                    line: 3,
                    note: "source: request.getParameter".into(),
                },
                Evidence {
                    file: file.clone(),
                    line: 4,
                    note: "concatenated into `sql`".into(),
                },
            ],
        },
        Finding {
            detector: Detector::Lint,
            rule: "java-catch-generic-exception".into(),
            file: file.clone(),
            line: 6,
            col: 23,
            function: Some("A::doPost".into()),
            message: "catch (Exception) swallows every error".into(),
            confidence: 0.35,
            evidence: Vec::new(),
        },
        Finding {
            detector: Detector::CodeQL,
            rule: "codeql::java/sql-injection".into(),
            file: "src/main/java/A.java".into(),
            line: 5,
            col: 30,
            function: None,
            message: "This query depends on a user-provided value.".into(),
            confidence: 0.8,
            evidence: Vec::new(),
        },
    ]
}

fn docs() -> BTreeMap<String, RuleDoc> {
    let mut docs = BTreeMap::new();
    docs.insert(
        "java-sqli-taint".to_string(),
        RuleDoc {
            id: "java-sqli-taint".into(),
            short: "SQL built from request input".into(),
            full: Some("A request parameter reaches SQL text unsanitized.".into()),
            tags: vec!["CWE-89".into(), "taint".into()],
            security_severity: Some(7.5),
            ..RuleDoc::default()
        },
    );
    docs.insert(
        "codeql::java/sql-injection".to_string(),
        RuleDoc {
            id: "codeql::java/sql-injection".into(),
            short: "Query built from user-controlled sources".into(),
            tags: vec!["security".into(), "external/cwe/cwe-089".into()],
            precision: Some("high".into()),
            security_severity: Some(8.8),
            ..RuleDoc::default()
        },
    );
    docs
}

fn project(source: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    std::fs::create_dir_all(root.join("src/main/java")).unwrap();
    std::fs::write(root.join("src/main/java/A.java"), source).unwrap();
    (dir, root)
}

fn export(root: &Path, findings: &[Finding]) -> Value {
    let tool = ToolInfo {
        version: "0.0.0-test".into(),
        ..ToolInfo::default()
    };
    to_sarif(findings, root, &docs(), &tool)
}

/// The log with the machine-specific root URI replaced.
fn normalized(mut log: Value) -> Value {
    log["runs"][0]["originalUriBaseIds"]["%SRCROOT%"]["uri"] = Value::from("file:///PROJECT/");
    log
}

#[test]
fn export_matches_the_golden_log_and_the_schema() {
    let (_dir, root) = project(SOURCE);
    let log = export(&root, &findings());
    let errors = schema_errors(&log);
    assert!(errors.is_empty(), "schema violations: {errors:#?}");

    let text = serde_json::to_string_pretty(&normalized(log)).unwrap() + "\n";
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN);
    if std::env::var_os("CODEGRAPH_UPDATE_GOLDEN").is_some() {
        std::fs::write(&golden, &text).unwrap();
    }
    let expected = std::fs::read_to_string(&golden)
        .expect("golden file (CODEGRAPH_UPDATE_GOLDEN=1 writes it)");
    assert_eq!(
        text, expected,
        "SARIF export changed; review and update the golden"
    );
}

#[test]
fn the_log_is_what_code_scanning_reads() {
    let (_dir, root) = project(SOURCE);
    let log = export(&root, &findings());
    let run = &log["runs"][0];
    let rules = run["tool"]["driver"]["rules"].as_array().unwrap();
    assert_eq!(rules.len(), 3, "one descriptor per rule id");
    let sqli = &rules[0];
    assert_eq!(sqli["id"], "java-sqli-taint");
    assert_eq!(
        sqli["shortDescription"]["text"],
        "SQL built from request input"
    );
    let tags: Vec<&str> = sqli["properties"]["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    assert!(tags.contains(&"external/cwe/cwe-089"), "{tags:?}");
    assert_eq!(sqli["properties"]["security-severity"], "7.5");
    assert!(
        rules[1]["shortDescription"]["text"]
            .as_str()
            .unwrap()
            .contains("Lint"),
        "a rule with no doc gets a fallback description"
    );

    let results = run["results"].as_array().unwrap();
    let first = &results[0];
    assert_eq!(first["level"], "error");
    assert_eq!(first["rank"], 90.0);
    assert_eq!(
        first["locations"][0]["physicalLocation"]["region"],
        serde_json::json!({"startLine": 5, "startColumn": 9})
    );
    assert_eq!(
        first["locations"][0]["logicalLocations"][0]["fullyQualifiedName"],
        "A::doPost"
    );
    let steps = first["codeFlows"][0]["threadFlows"][0]["locations"]
        .as_array()
        .unwrap();
    assert_eq!(steps.len(), 2, "evidence is the code flow");
    assert_eq!(results[1]["level"], "note");
    assert!(results[1].get("codeFlows").is_none());
}

#[test]
fn fingerprints_survive_moved_lines() {
    let prints = |source: &str| -> Vec<String> {
        let (_dir, root) = project(source);
        let mut findings = findings();
        let shift = source.lines().count() as u32 - SOURCE.lines().count() as u32;
        for finding in &mut findings {
            finding.line += shift;
        }
        let log = export(&root, &findings);
        log["runs"][0]["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                r["partialFingerprints"]["codegraphFingerprint/v1"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    };
    let before = prints(SOURCE);
    let after = prints(&format!("// a header\n\n{SOURCE}"));
    assert_eq!(before, after, "two lines above move nothing");
    let unique: std::collections::HashSet<&String> = before.iter().collect();
    assert_eq!(unique.len(), before.len(), "one print per result");
}

#[test]
fn the_reader_reads_back_what_the_writer_wrote() {
    let (_dir, root) = project(SOURCE);
    let log = export(&root, &findings());
    let runs = read::parse_value(&log).unwrap();
    let run = &runs[0];
    assert_eq!(run.tool, "codegraph");
    assert_eq!(run.results.len(), 3);
    assert_eq!(run.rules["java-sqli-taint"].cwes(), ["CWE-89"]);
    assert_eq!(run.rules["java-sqli-taint"].security_severity, Some(7.5));
    let sqli = &run.results[0];
    assert_eq!(sqli.rule_id, "java-sqli-taint");
    assert_eq!(sqli.level.as_deref(), Some("error"));
    let location = sqli.location.as_ref().unwrap();
    assert_eq!(
        location.relative_path(&root).as_deref(),
        Some("src/main/java/A.java")
    );
    assert_eq!((location.line, location.column), (5, 9));
    let hops: Vec<(u32, Option<&str>)> = sqli.flows[0]
        .iter()
        .map(|s| (s.line, s.message.as_deref()))
        .collect();
    assert_eq!(
        hops,
        [
            (3, Some("source: request.getParameter")),
            (4, Some("concatenated into `sql`"))
        ]
    );
}

#[test]
fn codeqls_own_sarif_passes_the_validator() {
    let sample: Value = serde_json::from_str(include_str!(
        "../src/analyze/codeql/samples/owasp-benchmark-java.sarif"
    ))
    .unwrap();
    let errors = schema_errors(&sample);
    assert!(errors.is_empty(), "{errors:#?}");
}

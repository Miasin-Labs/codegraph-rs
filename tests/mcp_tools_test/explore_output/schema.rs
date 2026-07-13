/// Minimal JSON-Schema-subset validator covering exactly the constructs the
/// codegraph tool output schemas use: oneOf, enum, const, type, properties,
/// required, additionalProperties:false, and items. No external dependency.
fn schema_matches(schema: &serde_json::Value, value: &serde_json::Value) -> bool {
    if let Some(branches) = schema.get("oneOf").and_then(|v| v.as_array()) {
        return branches.iter().any(|b| schema_matches(b, value));
    }
    if let Some(allowed) = schema.get("enum").and_then(|v| v.as_array()) {
        if !allowed.iter().any(|a| a == value) {
            return false;
        }
    }
    if let Some(constant) = schema.get("const") {
        if constant != value {
            return false;
        }
    }
    if let Some(ty) = schema.get("type").and_then(|v| v.as_str()) {
        let ok = match ty {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "integer" => value.is_i64() || value.is_u64(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            _ => true,
        };
        if !ok {
            return false;
        }
    }
    if let Some(obj) = value.as_object() {
        let properties = schema.get("properties").and_then(|v| v.as_object());
        let additional_allowed = schema
            .get("additionalProperties")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        if !additional_allowed {
            if let Some(props) = properties {
                if obj.keys().any(|key| !props.contains_key(key)) {
                    return false;
                }
            }
        }
        if let Some(required) = schema.get("required").and_then(|v| v.as_array()) {
            for req in required.iter().filter_map(|r| r.as_str()) {
                if !obj.contains_key(req) {
                    return false;
                }
            }
        }
        if let Some(props) = properties {
            for (key, subschema) in props {
                if let Some(child) = obj.get(key) {
                    if !schema_matches(subschema, child) {
                        return false;
                    }
                }
            }
        }
    }
    if let Some(arr) = value.as_array() {
        if let Some(items) = schema.get("items") {
            if arr.iter().any(|item| !schema_matches(items, item)) {
                return false;
            }
        }
    }
    true
}

fn explore_output_schema() -> serde_json::Value {
    get_static_tools()
        .into_iter()
        .find(|t| t.name == "codegraph_explore")
        .expect("codegraph_explore tool")
        .output_schema
        .expect("explore advertises an output schema")
}

#[tokio::test(flavor = "current_thread")]
async fn explore_v2_payload_validates_against_the_advertised_output_schema() {
    let _env = env_read().await;
    let schema = explore_output_schema();
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("src/state.ts"),
        "export function target(): number {\n  return compute(1);\n}\n\nfunction compute(n: number): number {\n  return n + 1;\n}\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let structured = handler
        .execute("codegraph_explore", &json!({ "query": "target compute" }))
        .structured_content
        .expect("structured explore");
    assert!(
        schema_matches(&schema, &structured),
        "v2 payload failed its advertised outputSchema: {structured}"
    );
    // Sanity: the payload really is the v2 shape we validated.
    assert_eq!(structured["schemaVersion"], 2);
    assert!(structured["sourceFiles"][0]["chunks"].is_array());
}

#[tokio::test(flavor = "current_thread")]
async fn explore_output_schema_rejects_legacy_header_body_payload() {
    let _env = env_read().await;
    let schema = explore_output_schema();
    // A pre-v2 payload that carried per-file header/body markdown must not pass
    // the v2 schema: header/body violate additionalProperties, chunks is missing.
    let legacy = json!({
        "schemaVersion": 1,
        "kind": "explore",
        "query": "target",
        "totalSymbols": 1,
        "totalFiles": 1,
        "filesIncluded": 1,
        "sourceFiles": [{
            "path": "src/state.ts",
            "language": "typescript",
            "header": "#### src/state.ts — target",
            "body": "export function target() {}"
        }],
        "relationships": [],
        "additionalFiles": [],
        "literalMatches": [],
        "trimmed": false,
        "filesOmitted": 0,
        "omissions": [],
        "continuation": { "suggestedQueries": [] }
    });
    assert!(
        !schema_matches(&schema, &legacy),
        "legacy header/body payload was accepted by the v2 schema"
    );
}

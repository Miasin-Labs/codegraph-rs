fn big_stale_file() -> String {
    let mut parts = Vec::new();
    for handler in 0..80 {
        parts.push(format!("/** handler number {handler} */"));
        parts.push(format!(
            "export function handler{handler}(input: string): string {{"
        ));
        for step in 0..8 {
            parts.push(format!(
                "  const v{step} = input + \"-step{step}-h{handler}\";"
            ));
        }
        parts.push("  return v7;".to_string());
        parts.push("}".to_string());
        parts.push(String::new());
    }
    parts.extend([
        "export function orchestrate(input: string): string {".to_string(),
        "  handler0(input);".to_string(),
        "  handler1(input);".to_string(),
        "  handler2(input);".to_string(),
        "  handler3(input);".to_string(),
        "  return input;".to_string(),
        "}".to_string(),
        String::new(),
    ]);
    parts.join("\n")
}

fn stale_prelude() -> String {
    let mut parts = Vec::new();
    for helper in 0..4 {
        parts.push(format!("/** inserted helper {helper} */"));
        parts.push(format!(
            "export function insertedHelper{helper}(x: number): number {{"
        ));
        for step in 0..7 {
            parts.push(format!("  x = x + {step};"));
        }
        parts.push("  return x;".to_string());
        parts.push("}".to_string());
    }
    parts.push(String::new());
    format!("{}\n", parts.join("\n"))
}

async fn stale_slice_fixture() -> (TempDir, TempDir, ToolHandler) {
    let fixture = TempDir::new().unwrap();
    let default = TempDir::new().unwrap();
    write(&fixture.path().join("src/big.ts"), &big_stale_file());
    write(
        &fixture.path().join("src/small.ts"),
        "export function smallTarget(n: number): number {\n  return n * 2;\n}\n",
    );
    write(
        &default.path().join("src/unrelated.ts"),
        "export function unrelated() { return 0; }\n",
    );
    let fixture_graph = CodeGraph::init_sync(fixture.path()).unwrap();
    fixture_graph
        .index_all(&IndexOptions::default())
        .await
        .unwrap();
    fixture_graph.close();
    let default_graph = CodeGraph::init_sync(default.path()).unwrap();
    default_graph
        .index_all(&IndexOptions::default())
        .await
        .unwrap();
    let handler = ToolHandler::new(Some(Rc::new(default_graph)));
    (fixture, default, handler)
}

#[tokio::test(flavor = "current_thread")]
async fn node_never_serves_another_symbols_body_from_a_drifted_project_path_file() {
    let _env = env_read().await;
    let (fixture, _default, handler) = stale_slice_fixture().await;
    let path = fixture.path().join("src/big.ts");
    write(
        &path,
        &(stale_prelude() + &fs::read_to_string(&path).unwrap()),
    );

    let result = handler.execute(
        "codegraph_node",
        &json!({
            "symbol": "orchestrate",
            "includeCode": true,
            "projectPath": fixture.path(),
        }),
    );

    assert_ne!(result.is_error, Some(true), "{}", result.text());
    assert!(!result.text().contains("-h76"), "{}", result.text());
    assert!(!result.text().contains("handler77"), "{}", result.text());
    assert!(
        result
            .text()
            .contains("changed on disk after it was last indexed")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn node_serves_full_current_source_for_a_small_drifted_file() {
    let _env = env_read().await;
    let (fixture, _default, handler) = stale_slice_fixture().await;
    let path = fixture.path().join("src/small.ts");
    write(
        &path,
        &("/** new first line */\nexport const shift = 1;\n".to_string()
            + &fs::read_to_string(&path).unwrap()),
    );

    let result = handler.execute(
        "codegraph_node",
        &json!({
            "symbol": "smallTarget",
            "includeCode": true,
            "projectPath": fixture.path(),
        }),
    );

    assert_ne!(result.is_error, Some(true), "{}", result.text());
    assert!(result.text().contains("full CURRENT source"));
    assert!(result.text().contains("new first line"));
    assert_eq!(result.meta.as_ref().unwrap().notices[0].kind, "stale_index");
}

#[tokio::test(flavor = "current_thread")]
async fn explore_omits_oversized_drifted_source_and_marks_stale_index() {
    let _env = env_read().await;
    let (fixture, _default, handler) = stale_slice_fixture().await;
    let path = fixture.path().join("src/big.ts");
    write(
        &path,
        &(stale_prelude() + &fs::read_to_string(&path).unwrap()),
    );

    let result = handler.execute(
        "codegraph_explore",
        &json!({
            "query": "orchestrate handler3",
            "projectPath": fixture.path(),
        }),
    );

    assert_ne!(result.is_error, Some(true), "{}", result.text());
    assert!(
        result
            .text()
            .contains("changed on disk after the last index sync")
    );
    assert!(!result.text().contains("-step3-h3"), "{}", result.text());
    let notices = &result.meta.as_ref().unwrap().notices;
    assert!(notices.iter().any(|notice| notice.kind == "stale_index"));
    let structured = result.structured_content.as_ref().unwrap();
    assert!(
        structured["omissions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| { item["path"] == "src/big.ts" && item["reason"] == "stale_index" })
    );
}

#[tokio::test(flavor = "current_thread")]
async fn explore_serves_full_current_source_for_a_small_drifted_file() {
    let _env = env_read().await;
    let (fixture, _default, handler) = stale_slice_fixture().await;
    let path = fixture.path().join("src/small.ts");
    write(
        &path,
        &("/** current prelude */\nexport const shifted = true;\n".to_string()
            + &fs::read_to_string(&path).unwrap()),
    );

    let result = handler.execute(
        "codegraph_explore",
        &json!({
            "query": "smallTarget",
            "projectPath": fixture.path(),
        }),
    );

    assert_ne!(result.is_error, Some(true), "{}", result.text());
    assert!(
        result.text().contains("current prelude"),
        "{}",
        result.text()
    );
    assert!(result.text().contains("source below is full and current"));
    assert_eq!(result.meta.as_ref().unwrap().notices[0].kind, "stale_index");
}

#[tokio::test(flavor = "current_thread")]
async fn identical_rewrite_does_not_trip_the_stale_slice_guard() {
    let _env = env_read().await;
    let (fixture, _default, handler) = stale_slice_fixture().await;
    let path = fixture.path().join("src/big.ts");
    write(&path, &fs::read_to_string(&path).unwrap());

    let result = handler.execute(
        "codegraph_node",
        &json!({
            "symbol": "orchestrate",
            "includeCode": true,
            "projectPath": fixture.path(),
        }),
    );

    assert!(
        !result.text().contains("changed on disk"),
        "{}",
        result.text()
    );
    assert!(result.text().contains("export function orchestrate"));
}

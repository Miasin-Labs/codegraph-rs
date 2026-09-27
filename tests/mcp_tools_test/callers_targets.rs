#[tokio::test(flavor = "current_thread")]
async fn callers_of_a_shared_name_say_which_definition_each_reaches() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    // Two unrelated types each have a `sync` method, called from different
    // places: a bare-name query must not read as one list.
    write(
        &src.join("cloud.ts"),
        "export class Cloud {\n  sync() { return 1; }\n}\n\
         export function pushCloud(c: Cloud) { return c.sync(); }\n",
    );
    write(
        &src.join("calendar.ts"),
        "export class Calendar {\n  sync() { return 2; }\n}\n\
         export function refreshCalendar(c: Calendar) { return c.sync(); }\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let result = handler.execute("codegraph_callers", &json!({ "symbol": "sync" }));
    let payload = result.structured_content.expect("structured");
    assert_eq!(payload["matches"].as_array().unwrap().len(), 2, "{payload:#}");
    let target_of = |caller: &str| {
        payload["results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == caller)
            .unwrap_or_else(|| panic!("{caller} in {payload:#}"))["target"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert!(target_of("pushCloud").contains("Cloud"), "{payload:#}");
    assert!(target_of("refreshCalendar").contains("Calendar"), "{payload:#}");
    assert!(schema_matches(&tool_output_schema("codegraph_callers"), &payload));

    // One definition: rows carry no target.
    let single = handler.execute("codegraph_callers", &json!({ "symbol": "Cloud.sync" }));
    let payload = single.structured_content.expect("structured");
    if payload["results"].as_array().is_some_and(|rows| !rows.is_empty()) {
        assert!(payload["results"][0].get("target").is_none(), "{payload:#}");
    }
}

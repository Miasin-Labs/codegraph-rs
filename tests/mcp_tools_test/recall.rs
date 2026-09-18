use codegraph::history::sources::{RawPrompt, RawSession, SourceEvent, SourceStats, Visit};
use codegraph::history::{
    CallResult,
    EventSource,
    HistoryDb,
    HistoryError,
    IncrementalOptions,
    RawToolCall,
    now_ms,
};

/// Replays fixed session events into the history store.
struct RecallReplay(Vec<SourceEvent>);

impl EventSource for RecallReplay {
    fn id(&self) -> &'static str {
        "replay"
    }

    fn location(&self) -> String {
        "fixture".into()
    }

    fn visit_events(
        &self,
        _visit: &Visit<'_>,
        sink: &mut dyn FnMut(SourceEvent) -> Result<(), HistoryError>,
    ) -> Result<SourceStats, HistoryError> {
        for e in &self.0 {
            sink(e.clone())?;
        }
        Ok(SourceStats::default())
    }
}

/// Every object in `value` carries only the properties `schema` declares,
/// and all it requires (what clients enforce with `additionalProperties: false`).
fn conforms(value: &serde_json::Value, schema: &serde_json::Value, at: &str) {
    match value {
        serde_json::Value::Object(map) => {
            let props = schema["properties"].as_object().unwrap_or_else(|| panic!("{at}: no properties in {schema}"));
            for key in map.keys() {
                let sub = props.get(key).unwrap_or_else(|| panic!("{at}.{key} is not declared"));
                conforms(&map[key], sub, &format!("{at}.{key}"));
            }
            for req in schema["required"].as_array().into_iter().flatten() {
                assert!(map.contains_key(req.as_str().unwrap()), "{at} lacks required {req}");
            }
        }
        serde_json::Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                conforms(item, &schema["items"], &format!("{at}[{i}]"));
            }
        }
        _ => {}
    }
}

fn recall_events(root: &Path) -> Vec<SourceEvent> {
    let at = now_ms() - 3_600_000;
    let cwd = root.to_string_lossy().into_owned();
    let call = |id: &str, dt: i64, tool: &str| RawToolCall {
        native_id: id.into(),
        ts_ms: Some(at + dt),
        session: Some("S".into()),
        tool: tool.into(),
        cwd: Some(cwd.clone()),
        ..Default::default()
    };
    vec![
        SourceEvent::Session(RawSession {
            native_id: "S".into(),
            cwd: Some(cwd.clone()),
            started_ms: Some(at),
            ..Default::default()
        }),
        SourceEvent::Prompt(RawPrompt {
            native_id: "S-1".into(),
            session: "S".into(),
            ts_ms: Some(at + 1),
        }),
        SourceEvent::call(RawToolCall {
            pattern: Some("parse_expr".into()),
            ..call("c1", 10, "Grep")
        }),
        SourceEvent::call(RawToolCall {
            file_path: Some(root.join("src/lib.rs").to_string_lossy().into_owned()),
            ..call("c2", 20, "Read")
        }),
        SourceEvent::call(RawToolCall {
            command: Some("cargo test".into()),
            result: Some(CallResult {
                is_error: Some(true),
                excerpt: Some("error[E0425]: cannot find value `y`".into()),
                ..Default::default()
            }),
            ..call("c3", 30, "Bash")
        }),
    ]
}

/// `codegraph_recall` is opt-in, answers from the history store within its
/// declared output schema, and never refreshes inline.
#[tokio::test(flavor = "current_thread")]
async fn recall_is_opt_in_and_answers_within_its_schema() {
    let _env = env_write().await;
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join(".git")).unwrap();
    write(&dir.path().join("src/lib.rs"), "pub fn parse_expr() -> u32 { 1 }\n");
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let root = cg.get_project_root().to_path_buf();

    let history = TempDir::new().unwrap();
    let db = history.path().join("history.db");
    let replay = RecallReplay(recall_events(&root));
    let opts = IncrementalOptions {
        time_budget: None,
        max_events: None,
        git: false,
        ..Default::default()
    };
    HistoryDb::open(&db).unwrap().ingest_events(&[&replay], &opts).unwrap();

    let _db = EnvVarGuard::set("CODEGRAPH_HISTORY_DB", &db.to_string_lossy());
    let _bg = EnvVarGuard::set("CODEGRAPH_NO_BACKGROUND_SYNC", "1");
    {
        let _tools = EnvVarGuard::unset("CODEGRAPH_MCP_TOOLS");
        let listed: Vec<String> = ToolHandler::new(None).get_tools().into_iter().map(|t| t.name).collect();
        assert!(!listed.iter().any(|n| n == "codegraph_recall"), "recall ships opt-in");
    }
    let _tools = EnvVarGuard::set("CODEGRAPH_MCP_TOOLS", "recall,explore");
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let def = handler
        .get_tools()
        .into_iter()
        .find(|t| t.name == "codegraph_recall")
        .expect("allowlisted recall is listed");
    let annotations = serde_json::to_value(&def.annotations).unwrap();
    assert_eq!(annotations["readOnlyHint"], true);
    let schema = serde_json::to_value(&def.output_schema).unwrap();
    let success = &schema["oneOf"][0];

    let res = handler.execute("codegraph_recall", &json!({ "about": "src/" }));
    assert_ne!(res.is_error, Some(true), "recall errored: {}", res.text());
    let payload = res.structured_content.clone().unwrap();
    conforms(&payload, success, "recall");
    assert_eq!(payload["kind"], "recall");
    assert_eq!(payload["episodes"][0]["files"][0]["path"], "src/lib.rs");
    assert_eq!(payload["episodes"][0]["outcome"], "tests-fail");
    assert!(serde_json::to_string(&payload).unwrap().len() <= 2048);

    let res = handler.execute("codegraph_recall", &json!({ "about": "symbol:parse_expr" }));
    let payload = res.structured_content.clone().unwrap();
    conforms(&payload, success, "recall");
    assert_eq!(payload["symbols"][0]["found"][0], "src/lib.rs:1", "{payload}");

    let res = handler.execute("codegraph_recall", &json!({ "about": "failures", "since": "7d" }));
    let payload = res.structured_content.clone().unwrap();
    conforms(&payload, success, "recall");
    assert_eq!(payload["failures"][0]["codes"][0], "E0425");
    assert_eq!(payload["failures"][0]["fixedLater"], false);

    let res = handler.execute("codegraph_recall", &json!({ "since": "soon" }));
    assert_eq!(res.is_error, Some(true), "a bad duration is a validation error");
}

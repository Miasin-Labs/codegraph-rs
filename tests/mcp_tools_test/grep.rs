/// A small Rust project where one log message appears in code, a comment,
/// a test, the README, and a config file.
async fn grep_fixture(root: &Path) -> ToolHandler {
    write(
        &root.join("src/net.rs"),
        "pub struct Conn {\n    retries: u32,\n}\n\n\
         impl Conn {\n    pub fn reset(&mut self) {\n        \
         // log: connection reset by peer\n        \
         eprintln!(\"connection reset by peer\");\n        \
         self.retries += 1;\n    }\n}\n",
    );
    write(
        &root.join("src/util.rs"),
        "pub fn describe() -> &'static str {\n    \"connection reset by peer\"\n}\n",
    );
    write(
        &root.join("tests/net_test.rs"),
        "#[test]\nfn reports_reset() {\n    assert!(\"connection reset by peer\".len() > 0);\n}\n",
    );
    write(
        &root.join("README.md"),
        "# Net\n\nLogs `connection reset by peer` when the socket drops.\n",
    );
    write(
        &root.join("config.toml"),
        "[net]\nmessage = \"connection reset by peer\"\n",
    );
    let cg = CodeGraph::init_sync(root).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    ToolHandler::new(Some(Rc::new(cg)))
}

fn grep_payload(handler: &ToolHandler, args: serde_json::Value) -> serde_json::Value {
    let result = handler.execute("codegraph_grep", &args);
    assert_ne!(result.is_error, Some(true), "grep errored: {}", result.text());
    let payload = result
        .structured_content
        .clone()
        .expect("grep returns a structured payload");
    assert_declared(&grep_schema(), &payload, "$");
    payload
}

fn grep_schema() -> serde_json::Value {
    let definition = tools()
        .into_iter()
        .find(|tool| tool.name == "codegraph_grep")
        .expect("codegraph_grep is registered");
    serde_json::to_value(definition).unwrap()["outputSchema"]["oneOf"][0].clone()
}

/// Every field of `value` is declared by `schema` (the payloads promise
/// `additionalProperties: false`, and clients reject undeclared fields).
fn assert_declared(schema: &serde_json::Value, value: &serde_json::Value, at: &str) {
    match value {
        serde_json::Value::Object(fields) => {
            let properties = &schema["properties"];
            for (key, field) in fields {
                let declared = &properties[key];
                assert!(!declared.is_null(), "{at}.{key} is not in the output schema");
                assert_declared(declared, field, &format!("{at}.{key}"));
            }
        }
        serde_json::Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                assert_declared(&schema["items"], item, &format!("{at}[{index}]"));
            }
        }
        _ => {}
    }
}

/// Every `N: text` / `N- text` line of a file row, in order.
fn lines_of(file: &serde_json::Value) -> Vec<String> {
    file["hits"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|group| group["lines"].as_array().unwrap().clone())
        .map(|line| line.as_str().unwrap().to_string())
        .collect()
}

fn files_of(payload: &serde_json::Value) -> Vec<String> {
    payload["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["file"].as_str().unwrap().to_string())
        .collect()
}

/// A literal log message is found in every indexed file that holds it, code
/// first, each hit on the symbol it sits in; config values are withheld.
#[tokio::test(flavor = "current_thread")]
async fn grep_ranks_code_first_and_places_hits_on_symbols() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let handler = grep_fixture(dir.path()).await;

    let payload = grep_payload(
        &handler,
        json!({ "pattern": "connection reset by peer", "literal": true }),
    );
    let files = files_of(&payload);
    assert_eq!(
        files,
        vec![
            "src/net.rs",
            "src/util.rs",
            "tests/net_test.rs",
            "README.md",
            "config.toml"
        ],
        "{payload}"
    );
    // Every file is listed with every hit, so no total repeats the rows.
    assert!(payload.get("totalFiles").is_none() && payload.get("totalHits").is_none());

    // Numbered `N: text` lines grouped under the symbol they sit in.
    let net = &payload["files"][0];
    assert_eq!(net["count"], 2, "{net}");
    assert!(net.get("more").is_none(), "{net}");
    let groups = net["hits"].as_array().unwrap();
    assert_eq!(groups.len(), 1, "{net}");
    assert_eq!(groups[0]["symbol"], "Conn::reset");
    assert_eq!(
        lines_of(net),
        vec![
            "7: // log: connection reset by peer",
            "8: eprintln!(\"connection reset by peer\");"
        ]
    );
    assert_eq!(payload["files"][1]["hits"][0]["symbol"], "describe");

    // The config file's line is reported, its value is not.
    let config = &payload["files"][4];
    assert_eq!(lines_of(config), vec!["2"], "{config}");
    assert_eq!(payload["valuesWithheld"], true);
    assert!(payload.get("nextCursor").is_none());
    assert!(payload.get("incomplete").is_none());
}

/// Regex syntax, grep's `\|`, case folding, and `path`/`glob` scoping.
#[tokio::test(flavor = "current_thread")]
async fn grep_takes_regexes_and_scopes() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let handler = grep_fixture(dir.path()).await;

    let payload = grep_payload(&handler, json!({ "pattern": r"retries \+= 1\|^pub fn describe" }));
    assert_eq!(files_of(&payload), vec!["src/net.rs", "src/util.rs"]);

    let payload = grep_payload(
        &handler,
        json!({ "pattern": "CONNECTION RESET", "caseInsensitive": true, "path": "src" }),
    );
    assert_eq!(files_of(&payload), vec!["src/net.rs", "src/util.rs"]);

    let payload = grep_payload(
        &handler,
        json!({ "pattern": "connection reset", "glob": "*.md" }),
    );
    assert_eq!(files_of(&payload), vec!["README.md"]);

    let payload = grep_payload(
        &handler,
        json!({ "pattern": "connection reset", "glob": "!*.{md,toml}", "path": "./tests/" }),
    );
    assert_eq!(files_of(&payload), vec!["tests/net_test.rs"]);

    // A glob passed as `path` is used as the glob.
    let payload = grep_payload(
        &handler,
        json!({ "pattern": "connection reset", "path": "src/*.rs" }),
    );
    assert_eq!(files_of(&payload), vec!["src/net.rs", "src/util.rs"]);

    let none = grep_payload(&handler, json!({ "pattern": "no such text anywhere" }));
    assert!(none["files"].as_array().unwrap().is_empty(), "{none}");

    // `maxPerFile` caps the lines shown per file; `more` says what is left.
    let capped = grep_payload(
        &handler,
        json!({ "pattern": "connection reset", "maxPerFile": 1, "path": "src/net.rs" }),
    );
    let net = &capped["files"][0];
    assert_eq!((net["count"].as_u64(), net["more"].as_u64()), (Some(2), Some(1)));
    // The line outside the comment is the one shown.
    assert_eq!(
        lines_of(net),
        vec!["8: eprintln!(\"connection reset by peer\");"]
    );

    // grep -w: `reset` is a word in these lines, `rese` is not.
    let word = grep_payload(&handler, json!({ "pattern": "rese", "word": true }));
    assert!(word["files"].as_array().unwrap().is_empty(), "{word}");
    let word = grep_payload(
        &handler,
        json!({ "pattern": "reset", "word": true, "path": "src" }),
    );
    assert_eq!(files_of(&word), vec!["src/net.rs", "src/util.rs"]);
}

/// grep -A/-B: numbered context around each hit, verbatim, snapping to a
/// small enclosing definition; -c and -l modes list rows without lines.
#[tokio::test(flavor = "current_thread")]
async fn grep_shows_context_and_count_and_files_modes() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let handler = grep_fixture(dir.path()).await;

    let payload = grep_payload(
        &handler,
        json!({ "pattern": "retries \\+=", "path": "src/net.rs", "after": 2 }),
    );
    let net = &payload["files"][0];
    assert_eq!(net["hits"][0]["symbol"], "Conn::reset");
    // `Conn::reset` (lines 6-10) is at most twice the 3-line window: shown whole.
    assert_eq!(
        lines_of(net),
        vec![
            "6-     pub fn reset(&mut self) {",
            "7-         // log: connection reset by peer",
            "8-         eprintln!(\"connection reset by peer\");",
            "9:         self.retries += 1;",
            "10-     }"
        ]
    );

    let counts = grep_payload(
        &handler,
        json!({ "pattern": "connection reset", "mode": "count" }),
    );
    let net = &counts["files"][0];
    assert_eq!((net["file"].as_str(), net["count"].as_u64()), (Some("src/net.rs"), Some(2)));
    assert!(net.get("hits").is_none());

    let names = grep_payload(
        &handler,
        json!({ "pattern": "connection reset", "mode": "files" }),
    );
    assert_eq!(names["files"].as_array().unwrap().len(), 5);
    assert!(names["files"][0].get("count").is_none(), "{names}");

    let bad = handler.execute(
        "codegraph_grep",
        &json!({ "pattern": "x", "mode": "everything" }),
    );
    assert_eq!(bad.is_error, Some(true));
}

/// Bad input gets a validation error that says what to fix.
#[tokio::test(flavor = "current_thread")]
async fn grep_rejects_bad_patterns_scopes_and_cursors() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let handler = grep_fixture(dir.path()).await;

    let bad = handler.execute("codegraph_grep", &json!({ "pattern": "reset(" }));
    assert_eq!(bad.is_error, Some(true));
    assert!(bad.text().contains("literal: true"), "{}", bad.text());

    let outside = handler.execute(
        "codegraph_grep",
        &json!({ "pattern": "x", "path": "Cargo.lock" }),
    );
    assert_eq!(outside.is_error, Some(true));
    assert!(outside.text().contains("No indexed file"), "{}", outside.text());

    let foreign = handler.execute(
        "codegraph_grep",
        &json!({ "pattern": "x", "cursor": "g1.0.-.0.0123456789" }),
    );
    assert_eq!(foreign.is_error, Some(true));
    assert!(foreign.text().contains("cursor"), "{}", foreign.text());
}

/// Past the row cap the remaining files are summarized by directory and a
/// cursor pages through them without repeating a file.
#[tokio::test(flavor = "current_thread")]
async fn grep_pages_many_files_with_a_cursor() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    for index in 0..55 {
        write(
            &dir.path().join(format!("src/m{}/f{index:02}.rs", index % 3)),
            &format!("pub fn f{index}() -> u32 {{\n    // MARKER {index}\n    {index}\n}}\n"),
        );
    }
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let first = grep_payload(&handler, json!({ "pattern": "MARKER", "limit": 5 }));
    assert_eq!(first["totalFiles"], 55);
    assert_eq!(first["truncated"], true);
    let listed = files_of(&first);
    assert_eq!(listed.len(), 40);
    let shown: usize = first["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["hits"].as_array().map_or(0, Vec::len))
        .sum();
    assert_eq!(shown, 5, "`limit` caps the hit lines");
    let summarized: u64 = first["dirs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|dir| dir["files"].as_u64().unwrap())
        .sum();
    assert_eq!(summarized, 15);

    let cursor = first["nextCursor"].as_str().unwrap();
    let second = grep_payload(
        &handler,
        json!({ "pattern": "MARKER", "limit": 5, "cursor": cursor }),
    );
    let rest = files_of(&second);
    assert_eq!(rest.len(), 15);
    assert!(rest.iter().all(|file| !listed.contains(file)));
    assert!(second.get("nextCursor").is_none());

    // A cursor belongs to its query.
    let other = handler.execute(
        "codegraph_grep",
        &json!({ "pattern": "OTHER", "cursor": cursor }),
    );
    assert_eq!(other.is_error, Some(true));
}

/// The wall-clock deadline is enforced: a search that runs out of time
/// returns what it has as `incomplete` with a cursor (having searched at
/// least one file, so paging always advances), and the cursor resumes at
/// the first file it did not search.
#[tokio::test(flavor = "current_thread")]
async fn grep_stops_at_its_deadline_and_resumes_from_the_cursor() {
    let _env = env_write().await;
    let dir = TempDir::new().unwrap();
    let handler = grep_fixture(dir.path()).await;

    let stopped = {
        let _deadline = EnvVarGuard::set("CODEGRAPH_GREP_DEADLINE_MS", "0");
        let started = std::time::Instant::now();
        let stopped = grep_payload(&handler, json!({ "pattern": "connection" }));
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        stopped
    };
    assert_eq!(stopped["incomplete"], true, "{stopped}");
    assert_eq!(stopped["searchedFiles"], 1);
    assert_eq!(stopped["candidateFiles"], 5);
    assert_eq!(files_of(&stopped), vec!["README.md"]);
    let cursor = stopped["nextCursor"].as_str().unwrap();

    let _default = EnvVarGuard::unset("CODEGRAPH_GREP_DEADLINE_MS");
    let resumed = grep_payload(&handler, json!({ "pattern": "connection", "cursor": cursor }));
    assert!(resumed.get("incomplete").is_none(), "{resumed}");
    assert!(resumed.get("nextCursor").is_none(), "{resumed}");
    assert_eq!(resumed["searchedFiles"], 4);
    let mut all = files_of(&stopped);
    all.extend(files_of(&resumed));
    all.sort();
    assert_eq!(
        all,
        vec![
            "README.md",
            "config.toml",
            "src/net.rs",
            "src/util.rs",
            "tests/net_test.rs"
        ]
    );
}

/// A byte budget smaller than the project stops at a file boundary.
#[tokio::test(flavor = "current_thread")]
async fn grep_stops_at_its_byte_budget() {
    let _env = env_write().await;
    let dir = TempDir::new().unwrap();
    let handler = grep_fixture(dir.path()).await;

    let _budget = EnvVarGuard::set("CODEGRAPH_GREP_MAX_BYTES", "1");
    let partial = grep_payload(&handler, json!({ "pattern": "connection" }));
    assert_eq!(partial["incomplete"], true, "{partial}");
    assert_eq!(partial["searchedFiles"], 1, "one file is always searched");
    assert!(partial["nextCursor"].is_string());
}

/// Grep ships in the default surface, read-only like the other lookups.
#[tokio::test(flavor = "current_thread")]
async fn grep_is_a_default_read_only_tool() {
    let _env = env_write().await;
    let _guard = EnvVarGuard::unset("CODEGRAPH_MCP_TOOLS");
    let grep = get_static_tools()
        .into_iter()
        .find(|tool| tool.name == "codegraph_grep")
        .expect("codegraph_grep is a default tool");
    let annotations = grep.annotations.expect("annotations");
    assert_eq!(annotations.read_only_hint, Some(true));
    assert!(grep.output_schema.is_some());
    assert_eq!(
        grep.input_schema.required.as_deref(),
        Some(&["pattern".to_string()][..])
    );
}

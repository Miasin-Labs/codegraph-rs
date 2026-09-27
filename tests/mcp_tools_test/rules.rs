// `codegraph_rules`: the loop a model writes bug rules in — variant, check,
// run, save, score — each answering within its declared output schema.

/// A sync whose `KeepBoth`-like arm (line 16) downloads a copy and yields
/// `None` while its sibling arms yield what their call returned (the rms
/// bug, in miniature), and `settle` does the same work but reports it.
const SYNC_RS: &str = "pub struct Engine;
pub struct Entry;

impl Engine {
    pub async fn upload_counted(&self) -> Option<Entry> { Some(Entry) }
    pub async fn download_counted(&self) -> Option<Entry> { Some(Entry) }
    pub async fn download_file(&self) -> Result<u32, String> { Ok(1) }

    pub async fn sync_both(&self, choice: u8) -> Option<Entry> {
        match choice {
            0 => {
                self.upload_counted()
                    .await
            }
            1 => self.download_counted().await,
            2 => {
                match self.download_file().await {
                    Ok(_) => {}
                    Err(_) => {}
                }
                None
            }
            _ => None,
        }
    }

    pub async fn settle(&self, choice: u8) -> Option<Entry> {
        match choice {
            0 => self.upload_counted().await,
            2 => {
                if self.download_file().await.is_err() {
                    return None;
                }
                self.upload_counted().await
            }
            _ => None,
        }
    }

    pub async fn size(&self) -> u32 {
        self.download_file().await.unwrap_or(0)
    }
}
";

/// The rule a model writes from the variant material (the demo's rule).
const ARM_RULE: &str = r#"id: arm-work-yields-none
description: A match arm does project work but yields None while a sibling arm yields its call's result.
severity: high
language: rust
message: "`{call}` runs in an arm that yields `None` in {function}"
check-patterns:
  - name: self-call-in-none-arm
    query: |
      (call_expression
        function: (field_expression value: (self) field: (field_identifier) @method)) @call
    where:
      - capture: call
        resolves-to: '^\w+::\w+$'
      - capture: call
        inside: |
          ((match_arm value: (block (identifier) @tail .)) (#eq? @tail "None"))
      - capture: call
        inside: |
          (match_block
            (match_arm value: [
              (await_expression (call_expression function: (field_expression value: (self))))
              (block (await_expression (call_expression function: (field_expression value: (self)))) .)]))
examples:
  bad:
    - code: |
        async fn f(&self, c: u8) -> Option<Entry> {
            match c {
                0 => self.upload_counted().await,
                _ => {
                    self.download_file().await;
                    None
                }
            }
        }
      resolves:
        self.download_file: Engine::download_file
  good:
    - code: |
        async fn f(&self, c: u8) -> Option<Entry> {
            match c {
                0 => self.upload_counted().await,
                _ => {
                    self.download_file().await;
                    self.upload_counted().await
                }
            }
        }
      resolves:
        self.download_file: Engine::download_file
"#;

/// Every object carries only the properties its schema declares (or what
/// `additionalProperties` allows), and all it requires.
fn rules_conforms(value: &serde_json::Value, schema: &serde_json::Value, at: &str) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(extra) = schema.get("additionalProperties").filter(|v| v.is_object()) {
                for (key, item) in map {
                    rules_conforms(item, extra, &format!("{at}.{key}"));
                }
                return;
            }
            let props = schema["properties"]
                .as_object()
                .unwrap_or_else(|| panic!("{at}: no properties in {schema}"));
            for key in map.keys() {
                let sub = props
                    .get(key)
                    .unwrap_or_else(|| panic!("{at}.{key} is not declared"));
                rules_conforms(&map[key], sub, &format!("{at}.{key}"));
            }
            for req in schema["required"].as_array().into_iter().flatten() {
                assert!(map.contains_key(req.as_str().unwrap()), "{at} lacks required {req}");
            }
        }
        serde_json::Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                rules_conforms(item, &schema["items"], &format!("{at}[{i}]"));
            }
        }
        _ => {}
    }
}

/// The success branch of the rules schema whose `kind` is `kind`.
fn rules_branch(schema: &serde_json::Value, kind: &str) -> serde_json::Value {
    schema["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|branch| branch["properties"]["kind"]["const"] == kind)
        .unwrap_or_else(|| panic!("no {kind} branch"))
        .clone()
}

async fn rules_fixture() -> (TempDir, CodeGraph) {
    let dir = TempDir::new().unwrap();
    write(&dir.path().join("src/sync.rs"), SYNC_RS);
    write(&dir.path().join("src/lib.rs"), "pub mod sync;\n");
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    (dir, cg)
}

/// `rules` ships opt-in, is not read-only (save, score write), and each
/// action answers in its own schema branch: variant → check → run → save →
/// run the saved rule.
#[tokio::test(flavor = "current_thread")]
async fn rules_tool_drives_the_rule_writing_loop() {
    let _env = env_write().await;
    let _bg = EnvVarGuard::set("CODEGRAPH_NO_BACKGROUND_SYNC", "1");
    let (dir, cg) = rules_fixture().await;
    {
        let _tools = EnvVarGuard::unset("CODEGRAPH_MCP_TOOLS");
        let listed: Vec<String> = ToolHandler::new(None).get_tools().into_iter().map(|t| t.name).collect();
        assert!(!listed.iter().any(|n| n == "codegraph_rules"), "rules ships opt-in");
    }
    let _tools = EnvVarGuard::set("CODEGRAPH_MCP_TOOLS", "rules,explore");
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let def = handler
        .get_tools()
        .into_iter()
        .find(|t| t.name == "codegraph_rules")
        .expect("allowlisted rules is listed");
    let annotations = serde_json::to_value(&def.annotations).unwrap();
    assert_eq!(annotations["readOnlyHint"], false);
    assert_eq!(annotations["destructiveHint"], false);
    let schema = serde_json::to_value(&def.output_schema).unwrap();

    // variant: the arm at line 16 → function, tree, resolved calls, a
    // skeleton that already passes check, and the deviance finding there.
    let res = handler.execute("codegraph_rules", &json!({ "action": "variant", "at": "src/sync.rs:16" }));
    assert_ne!(res.is_error, Some(true), "variant errored: {}", res.text());
    let variant = res.structured_content.clone().unwrap();
    rules_conforms(&variant, &rules_branch(&schema, "ruleVariant"), "variant");
    assert_eq!(variant["node"]["kind"], "match_arm");
    assert_eq!(variant["node"]["startLine"], 16);
    assert_eq!(variant["function"]["name"], "Engine::sync_both");
    assert!(
        variant["function"]["source"].as_str().unwrap().contains("16\t            2 => {"),
        "{variant}"
    );
    let tree = variant["node"]["tree"].as_str().unwrap();
    assert!(tree.starts_with("(match_arm\n  pattern: (match_pattern"), "{tree}");
    assert!(tree.contains("value: (block"), "{tree}");
    let calls = variant["calls"].as_array().unwrap();
    assert!(
        calls.iter().any(|c| c["callee"] == "self.download_file"
            && c["resolvesTo"][0] == "Engine::download_file"),
        "{calls:?}"
    );
    assert_eq!(variant["skeletonCheck"]["passed"], true, "{variant}");
    let skeleton = variant["skeleton"].as_str().unwrap();
    assert!(skeleton.contains("resolves-to: '^Engine::download_file$'"), "{skeleton}");
    assert!(skeleton.contains("(match_arm)"), "{skeleton}");
    assert!(skeleton.contains("'self.download_file': 'Engine::download_file'"), "{skeleton}");
    assert!(
        variant["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["rule"] == "arm-result-deviance" && !f["evidence"].as_array().unwrap().is_empty()),
        "{variant}"
    );
    // A place outside the file is an error, not a panic.
    let res = handler.execute("codegraph_rules", &json!({ "action": "variant", "at": "src/sync.rs:999" }));
    assert_eq!(res.is_error, Some(true));
    let res = handler.execute("codegraph_rules", &json!({ "action": "variant" }));
    assert_eq!(res.is_error, Some(true));
    let res = handler.execute("codegraph_rules", &json!({ "action": "variant", "at": "/etc/passwd:1" }));
    assert_eq!(res.is_error, Some(true), "no reads outside the project");

    // check: per-example outcomes; a failing example says why.
    let res = handler.execute("codegraph_rules", &json!({ "action": "check", "yaml": ARM_RULE }));
    let check = res.structured_content.clone().unwrap();
    rules_conforms(&check, &rules_branch(&schema, "ruleCheck"), "check");
    assert_eq!(check["passed"], 1, "{check}");
    assert_eq!(check["rules"][0]["examples"][0]["example"], "bad[0]");
    assert!(check["rules"][0]["examples"][0].get("why").is_none());
    let loose = ARM_RULE.replace("0 => self.upload_counted().await,\n                _ => {\n                    self.download_file().await;\n                    self.upload_counted().await", "0 => self.upload_counted().await,\n                _ => {\n                    self.download_file().await;\n                    None");
    let res = handler.execute("codegraph_rules", &json!({ "action": "check", "yaml": loose }));
    let check = res.structured_content.clone().unwrap();
    assert_eq!(check["failed"], 1, "{check}");
    let why = check["rules"][0]["examples"][1]["why"].as_str().unwrap();
    assert!(why.starts_with("good[0] matched check-patterns[0] `self-call-in-none-arm`"), "{why}");

    // run: the rule finds the arm in sync_both, not settle's reporting arm.
    let res = handler.execute("codegraph_rules", &json!({ "action": "run", "yaml": ARM_RULE }));
    assert_ne!(res.is_error, Some(true), "run errored: {}", res.text());
    let run = res.structured_content.clone().unwrap();
    rules_conforms(&run, &rules_branch(&schema, "ruleRun"), "run");
    assert_eq!(run["total"], 1, "{run}");
    assert_eq!(run["findings"][0]["file"], "src/sync.rs");
    assert_eq!(run["findings"][0]["line"], 17);
    assert_eq!(run["findings"][0]["function"], "Engine::sync_both");
    assert_eq!(run["byRule"]["arm-work-yields-none"], 1);

    // save: refused while an example fails; written once it passes; then
    // every run (and check) picks the saved rule up.
    let res = handler.execute("codegraph_rules", &json!({ "action": "save", "yaml": loose }));
    assert_eq!(res.is_error, Some(true));
    assert!(res.text().contains("does not pass check"), "{}", res.text());
    let res = handler.execute("codegraph_rules", &json!({ "action": "save", "yaml": ARM_RULE }));
    assert_ne!(res.is_error, Some(true), "save errored: {}", res.text());
    let save = res.structured_content.clone().unwrap();
    rules_conforms(&save, &rules_branch(&schema, "ruleSave"), "save");
    assert_eq!(save["path"], ".codegraph/rules/arm-work-yields-none.yaml");
    assert_eq!(save["replaced"], false);
    assert!(dir.path().join(".codegraph/rules/arm-work-yields-none.yaml").is_file());
    let res = handler.execute("codegraph_rules", &json!({ "action": "save", "yaml": ARM_RULE }));
    assert_eq!(res.structured_content.unwrap()["replaced"], true);

    let res = handler.execute("codegraph_rules", &json!({ "action": "run" }));
    let run = res.structured_content.clone().unwrap();
    assert_eq!(run["total"], 1, "the saved rule runs: {run}");
    let res = handler.execute("codegraph_rules", &json!({ "action": "run", "saved": false }));
    assert_eq!(res.is_error, Some(true), "nothing to run without the saved rules");
    let res = handler.execute("codegraph_rules", &json!({ "action": "check" }));
    assert_eq!(res.structured_content.unwrap()["passed"], 1);

    let res = handler.execute("codegraph_rules", &json!({ "action": "frobnicate" }));
    assert_eq!(res.is_error, Some(true));
}

/// A tiny labeled corpus: six functions that `eval` their input (bad) and
/// six that parse it (good), one function-granularity row each.
fn write_eval_corpus(root: &Path) {
    let mut source = String::new();
    let mut rows = String::new();
    for i in 0..6 {
        let line = source.lines().count() + 1;
        source.push_str(&format!("def bad_{i}(data):\n    return eval(data)\n\n"));
        rows.push_str(&format!(
            "{{\"file\": \"app.py\", \"label\": \"bad\", \"granularity\": \"function\", \"line_start\": {line}, \"line_end\": {}, \"cwe\": \"CWE-95\"}}\n",
            line + 1
        ));
    }
    for i in 0..6 {
        let line = source.lines().count() + 1;
        source.push_str(&format!("def good_{i}(data):\n    return int(data)\n\n"));
        rows.push_str(&format!(
            "{{\"file\": \"app.py\", \"label\": \"good\", \"granularity\": \"function\", \"line_start\": {line}, \"line_end\": {}, \"cwe\": \"CWE-95\"}}\n",
            line + 1
        ));
    }
    write(&root.join("app.py"), &source);
    write(&root.join("ground_truth.jsonl"), &rows);
}

const EVAL_RULES: &str = r#"id: py-eval
language: python
check-patterns:
  - query: |
      ((call function: (identifier) @f) @call (#eq? @f "eval"))
examples:
  bad: ["eval(x)"]
  good: ["int(x)"]
---
id: py-int
language: python
check-patterns:
  - query: |
      ((call function: (identifier) @f) @call (#eq? @f "int"))
examples:
  bad: ["int(x)"]
  good: ["eval(x)"]
"#;

/// `score` stages and indexes the corpus, scores each rule against the
/// ground truth, and keeps only the rule above the base rate.
#[tokio::test(flavor = "current_thread")]
async fn rules_score_keeps_only_rules_above_the_base_rate() {
    let _env = env_write().await;
    let _bg = EnvVarGuard::set("CODEGRAPH_NO_BACKGROUND_SYNC", "1");
    let _bin = EnvVarGuard::set("CODEGRAPH_SCORE_BIN", env!("CARGO_BIN_EXE_codegraph"));
    let _tools = EnvVarGuard::set("CODEGRAPH_MCP_TOOLS", "rules");
    let (_dir, cg) = rules_fixture().await;
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let schema = serde_json::to_value(
        handler
            .get_tools()
            .into_iter()
            .find(|t| t.name == "codegraph_rules")
            .unwrap()
            .output_schema,
    )
    .unwrap();
    let bench = TempDir::new().unwrap();
    let corpus = bench.path().join("evals");
    write_eval_corpus(&corpus);
    let work = bench.path().join("work");
    let args = json!({
        "action": "score", "yaml": EVAL_RULES, "saved": false,
        "corpus": corpus, "work": work, "wait": 50
    });
    let res = handler.execute("codegraph_rules", &args);
    assert_ne!(res.is_error, Some(true), "score errored: {}", res.text());
    let score = res.structured_content.clone().unwrap();
    rules_conforms(&score, &rules_branch(&schema, "ruleScore"), "score");
    assert_eq!(score["scoring"], "labeled");
    assert_eq!(score["complete"], true, "{score}");
    assert_eq!(score["units"]["done"], 1);
    assert_eq!(score["baseRate"], 0.5);
    assert_eq!(score["positives"], 6);
    let rule = |id: &str| {
        score["rules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .unwrap()
            .clone()
    };
    let eval = rule("py-eval");
    assert_eq!((eval["tp"].as_u64(), eval["fp"].as_u64()), (Some(6), Some(0)), "{eval}");
    assert_eq!(eval["precision"], 1.0);
    assert_eq!(eval["recall"], 1.0);
    assert_eq!(eval["verdict"], "keep", "{eval}");
    let int = rule("py-int");
    assert_eq!((int["tp"].as_u64(), int["fp"].as_u64()), (Some(0), Some(6)), "{int}");
    assert_eq!(int["verdict"], "discard");
    assert!(int["reason"].as_str().unwrap().contains("not 10 pts above the base rate"), "{int}");

    // A second score reuses the indexed unit and the cached findings.
    let res = handler.execute("codegraph_rules", &args);
    let again = res.structured_content.unwrap();
    assert_eq!(again["units"]["indexed"], 0, "{again}");
    assert_eq!(again["rules"], score["rules"]);

    let res = handler.execute("codegraph_rules", &json!({ "action": "score", "yaml": EVAL_RULES }));
    assert_eq!(res.is_error, Some(true), "score needs a corpus");
}

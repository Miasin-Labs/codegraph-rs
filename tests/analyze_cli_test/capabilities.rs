use std::process::{Command, Stdio};

#[test]
fn analyze_capabilities_lists_env_toggles_and_cascades() {
    // Pure environment read — no init required.
    let (_dir, root) = temp_project();

    let json = run_analyze_json(&root, &["capabilities"]);
    let capabilities = json["capabilities"].as_array().unwrap();
    assert_eq!(capabilities.len(), 6);
    let call_graph = capabilities
        .iter()
        .find(|c| c["name"].as_str() == Some("callGraph"))
        .expect("callGraph listed");
    assert_eq!(
        call_graph["envVar"].as_str(),
        Some("CODEGRAPH_ANALYSIS_CAP_CALL_GRAPH")
    );
    assert_eq!(call_graph["enabled"].as_bool(), Some(true));
    assert_eq!(
        call_graph["disables"][0].as_str(),
        Some("virtualValidation"),
        "dependency cascade surfaced: {call_graph}"
    );

    // A kill-switch env var disables the capability AND its dependents.
    let out = Command::new(bin())
        .args(["analyze", "capabilities", "--json"])
        .current_dir(&root)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_ANALYSIS_CAP_CALL_GRAPH", "0")
        .stdin(Stdio::null())
        .output()
        .expect("spawn codegraph binary");
    assert!(out.status.success());
    let envelope: serde_json::Value = serde_json::from_str(stdout_str(&out).trim()).unwrap();
    let capabilities = envelope["data"]["capabilities"].as_array().unwrap();
    let by_name = |name: &str| -> &serde_json::Value {
        capabilities
            .iter()
            .find(|c| c["name"].as_str() == Some(name))
            .unwrap()
    };
    assert_eq!(by_name("callGraph")["enabled"].as_bool(), Some(false));
    assert_eq!(by_name("callGraph")["envValue"].as_str(), Some("0"));
    assert_eq!(
        by_name("virtualValidation")["enabled"].as_bool(),
        Some(false),
        "cascade applied: {envelope}"
    );
    assert_eq!(by_name("typeUsage")["enabled"].as_bool(), Some(true));
}

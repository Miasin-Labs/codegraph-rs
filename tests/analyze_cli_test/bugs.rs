#[test]
fn analyze_bugs_reports_findings_and_review_packets_quote_them() {
    let (_dir, root) = temp_project();
    support::write(
        &root.join("src/layout.ts"),
        "export interface Size {\n  width: number;\n  height: number;\n}\n\n\
         export function shrink(size: Size, width: number): number {\n  \
         if (size.width < size.width) {\n    return width;\n  }\n  return size.width;\n}\n\n\
         export function layout(size: Size): number {\n  return shrink(size, 10) + 1;\n}\n",
    );
    init_fixture_files_only(&root);

    let report = run_analyze_json(&root, &["bugs"]);
    let findings = report["findings"].as_array().unwrap();
    let finding = findings
        .iter()
        .find(|finding| finding["rule"] == "self-comparison")
        .unwrap_or_else(|| panic!("self-comparison in {report}"));
    assert_eq!(finding["file"], "src/layout.ts");
    assert_eq!(finding["line"], 7);
    assert_eq!(finding["detector"], "lint");
    assert_eq!(report["byRule"]["self-comparison"], 1, "{report}");

    // One family at a time; an unknown one is an error that names the known.
    let deviance = run_analyze_json(&root, &["bugs", "--detector", "deviance"]);
    assert!(
        deviance["findings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|finding| finding["detector"] == "deviance"),
        "{deviance}"
    );
    let out = run_cli(&root, &["analyze", "bugs", "--detector", "nope"]);
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("deviance, lint"), "{}", stderr_str(&out));

    // The review packet quotes the function, names its caller, and asks.
    let packets = run_analyze_json(&root, &["review", "--at", "src/layout.ts:7"]);
    let packets = packets.as_array().unwrap();
    assert_eq!(packets.len(), 1, "{packets:?}");
    let packet = &packets[0];
    assert_eq!(packet["finding"]["rule"], "self-comparison");
    let function = &packet["function"];
    assert_eq!(function["startLine"], 6, "{packet}");
    assert!(
        function["source"]
            .as_str()
            .unwrap()
            .contains("7\t  if (size.width < size.width) {"),
        "{packet}"
    );
    assert!(
        packet["callers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|caller| caller["name"] == "layout"),
        "{packet}"
    );
    assert!(packet["checklist"].as_array().unwrap().len() >= 2);

    // Nothing selected is not an error.
    let none = run_analyze_json(&root, &["review", "--rule", "dead-store"]);
    assert_eq!(none.as_array().unwrap().len(), 0, "{none}");

    // The human forms print the same.
    let out = run_cli(&root, &["analyze", "review", "--at", "src/layout.ts"]);
    assert!(out.status.success(), "{}", stderr_str(&out));
    assert!(stdout_str(&out).contains("self-comparison"), "{}", stdout_str(&out));
    let out = run_cli(&root, &["analyze", "bugs"]);
    assert!(stdout_str(&out).contains("src/layout.ts:7"), "{}", stdout_str(&out));
}

#[test]
fn analyze_bugs_json_is_unbounded_unless_top_is_given() {
    let (_dir, root) = temp_project();
    // 55 self-comparisons: more than the 50 the human form shows.
    let source: String = (0..55)
        .map(|i| {
            format!(
                "export function f{i}(w: number): number {{\n  if (w < w) {{\n    return 1;\n  }}\n  return w;\n}}\n\n"
            )
        })
        .collect();
    support::write(&root.join("src/many.ts"), &source);
    init_fixture_files_only(&root);

    let report = run_analyze_json(&root, &["bugs"]);
    let all = report["findings"].as_array().unwrap().len();
    assert!(all >= 55, "{report}");
    assert_eq!(report["findingsOmitted"], 0, "{report}");

    // An explicit --top still applies to JSON.
    let report = run_analyze_json(&root, &["bugs", "--top", "3"]);
    assert_eq!(report["findings"].as_array().unwrap().len(), 3, "{report}");
    assert_eq!(report["findingsOmitted"], all - 3, "{report}");

    // A person reading the list gets the 50 most confident.
    let out = run_cli(&root, &["analyze", "bugs"]);
    assert!(out.status.success(), "{}", stderr_str(&out));
    let text = stdout_str(&out);
    assert!(text.contains(&format!("Suspected bugs: 50 of {all}")), "{text}");
    assert!(text.contains("raise --top"), "{text}");
}

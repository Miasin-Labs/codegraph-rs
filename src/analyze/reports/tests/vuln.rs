use super::{VulnReport, sample_vuln_report};

#[test]
fn sarif_has_rules_results_and_levels() {
    let sarif = sample_vuln_report().to_sarif();
    assert_eq!(sarif["version"], "2.1.0");
    let run = &sarif["runs"][0];
    assert_eq!(run["tool"]["driver"]["name"], "codegraph");

    // One rule per distinct template, sorted & deduped.
    let rules = run["tool"]["driver"]["rules"].as_array().unwrap();
    let rule_ids: Vec<&str> = rules.iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(rule_ids, vec!["missing_dominator_check", "must_follow"]);

    let results = run["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    // High confidence (0.91) escalates to SARIF `error`.
    assert_eq!(results[0]["ruleId"], "missing_dominator_check");
    assert_eq!(results[0]["level"], "error");
    assert_eq!(
        results[0]["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
        "src/handlers/order.rs"
    );
    assert_eq!(
        results[0]["locations"][0]["physicalLocation"]["region"]["startLine"],
        42
    );
    assert_eq!(results[0]["properties"]["confidence"], 0.91);
    // The MustFollow template is now emitted (was dead) from concurrency.
    assert_eq!(results[1]["ruleId"], "must_follow");
}

#[test]
fn html_report_has_summary_and_escapes() {
    let html = sample_vuln_report().to_html();
    assert!(html.starts_with("<!DOCTYPE html>"));
    assert!(html.contains("codegraph vulnerability report"));
    // Summary cards reflect the counts.
    assert!(html.contains("12 scanned function(s)"));
    assert!(html.contains("delete_order"));
    // Raw angle brackets in a message must be escaped, not emitted verbatim.
    assert!(html.contains("&lt;tag&gt;"));
    assert!(!html.contains("& <tag>"));
    // Both findings rendered as rows.
    assert!(html.contains("src/handlers/order.rs:42"));
    assert!(html.contains("src/queue.rs:7"));
}

#[test]
fn empty_vuln_report_html_is_well_formed() {
    let report = VulnReport {
        findings: vec![],
        missing_guard_count: 0,
        taint_count: 0,
        concurrency_count: 0,
        scanned_functions: 3,
    };
    let html = report.to_html();
    assert!(html.contains("No findings at or above"));
    assert!(html.trim_end().ends_with("</html>"));
}

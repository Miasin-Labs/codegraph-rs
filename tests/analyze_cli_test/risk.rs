#[test]
fn analyze_risk_ranks_changed_functions_no_test_reaches_first() {
    let (_dir, root) = temp_project();
    support::write(
        &root.join("src/lib.ts"),
        "export function covered(x: number): number {\n  return x + 1;\n}\n\n\
         export function uncovered(x: number): number {\n  return helper(x) * 2;\n}\n\n\
         export function helper(x: number): number {\n  return x;\n}\n",
    );
    support::write(
        &root.join("tests/lib.test.ts"),
        "import { covered } from '../src/lib';\n\n\
         export function testCovered(): void {\n  covered(1);\n}\n",
    );

    // Not a git repository: an error that says so, not an empty report.
    init_fixture_files_only(&root);
    let out = run_cli(&root, &["analyze", "risk"]);
    assert!(!out.status.success(), "stdout: {}", stdout_str(&out));
    assert!(stderr_str(&out).contains("git"), "{}", stderr_str(&out));

    git(&root, &["init", "-q"]);
    git(&root, &["add", "."]);
    git(&root, &["commit", "-qm", "base"]);
    // Change the library (not the test) after the base commit.
    let lib = root.join("src/lib.ts");
    let changed = std::fs::read_to_string(&lib)
        .unwrap()
        .replace("x + 1", "x + 2")
        .replace("* 2", "* 3");
    std::fs::write(&lib, changed).unwrap();
    let out = run_cli(&root, &["sync"]);
    assert!(out.status.success(), "sync failed: {}", stderr_str(&out));

    let report = run_analyze_json(&root, &["risk"]);
    assert_eq!(report["base"], "HEAD");
    assert_eq!(report["changedFiles"], 1, "{report}");
    let entries = report["entries"].as_array().unwrap();
    let by_name = |name: &str| {
        entries
            .iter()
            .find(|entry| entry["name"] == name)
            .unwrap_or_else(|| panic!("{name} in {report}"))
    };
    // The test file itself is never measured.
    assert!(entries.iter().all(|entry| entry["file"] == "src/lib.ts"));
    assert!(by_name("covered")["tests"].as_u64().unwrap() >= 1, "{report}");
    assert_eq!(by_name("uncovered")["tests"], 0, "{report}");
    // Untested functions rank ahead of tested ones.
    let first_tested = entries
        .iter()
        .position(|entry| entry["tests"].as_u64().unwrap() > 0)
        .unwrap();
    assert!(
        entries[..first_tested]
            .iter()
            .all(|entry| entry["tests"] == 0),
        "{report}"
    );
    assert_eq!(
        report["untested"].as_u64().unwrap() as usize,
        first_tested,
        "{report}"
    );

    // --untested keeps only those, and counts what it left out.
    let untested = run_analyze_json(&root, &["risk", "--untested"]);
    let listed = untested["entries"].as_array().unwrap();
    assert!(listed.iter().all(|entry| entry["tests"] == 0));
    assert_eq!(
        listed.len() as u64 + untested["entriesOmitted"].as_u64().unwrap(),
        untested["functions"].as_u64().unwrap()
    );

    // The human report names the gap.
    let out = run_cli(&root, &["analyze", "risk"]);
    assert!(out.status.success(), "{}", stderr_str(&out));
    assert!(stdout_str(&out).contains("uncovered"), "{}", stdout_str(&out));
}

fn init_fixture_files_only(root: &std::path::Path) {
    let out = run_cli(root, &["init"]);
    assert!(out.status.success(), "init failed: {}", stderr_str(&out));
}

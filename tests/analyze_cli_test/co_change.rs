use std::fs;

#[test]
fn analyze_co_change_mines_git_history_and_is_honest_without_it() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    // Phase 1: not a git repository → exit 0 with the honest note.
    let out = run_cli(&root, &["analyze", "co-change"]);
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert!(
        stdout_str(&out).contains("No git history"),
        "honest no-history note: {}",
        stdout_str(&out)
    );

    // Phase 2: util.ts and main.ts committed together twice → a real
    // cross-file pair at min support 2.
    git(&root, &["init", "-q"]);
    git(&root, &["add", "src"]);
    git(&root, &["commit", "-qm", "one"]);
    for (file, suffix) in [
        ("src/util.ts", "// touch a\n"),
        ("src/main.ts", "// touch b\n"),
    ] {
        let path = root.join(file);
        let mut content = fs::read_to_string(&path).unwrap();
        content.push_str(suffix);
        fs::write(&path, content).unwrap();
    }
    git(&root, &["add", "src"]);
    git(&root, &["commit", "-qm", "two"]);

    let json = run_analyze_json(&root, &["co-change", "--min-support", "2"]);
    assert_eq!(json["commitsAnalyzed"].as_u64(), Some(2));
    assert_eq!(json["minSupport"].as_u64(), Some(2));
    let pairs = json["pairs"].as_array().expect("pairs array");
    assert!(
        !pairs.is_empty(),
        "main.ts/util.ts symbols co-change twice: {json}"
    );
    let pair = &pairs[0];
    assert_eq!(pair["timesChangedTogether"].as_u64(), Some(2));
    assert!(pair["confidence"].as_f64().unwrap() > 0.0);
    // Pairs are cross-file by contract; same-file pairs are only counted.
    assert_ne!(
        pair["a"]["file"].as_str(),
        pair["b"]["file"].as_str(),
        "listed pairs are cross-file: {pair}"
    );
    assert!(json["sameFilePairCount"].as_u64().unwrap() > 0);

    // Seeded: every pair touches the seed symbol.
    let json = run_analyze_json(&root, &["co-change", "helper", "--min-support", "2"]);
    for pair in json["pairs"].as_array().unwrap() {
        assert!(
            pair["a"]["name"].as_str() == Some("helper")
                || pair["b"]["name"].as_str() == Some("helper"),
            "seeded pair touches helper: {pair}"
        );
    }
}

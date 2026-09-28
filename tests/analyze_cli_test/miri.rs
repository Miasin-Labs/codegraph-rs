/// A crate with real UB for Miri: an uninitialized read after `set_len`
/// (a test reaches it), a cast-pointer read no test reaches (a harness
/// must), an FFI call Miri cannot run, and sound unsafe code.
fn write_miri_fixture(root: &std::path::Path) {
    support::write(
        &root.join("Cargo.toml"),
        "[package]\nname = \"ubdemo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    );
    support::write(
        &root.join("src/lib.rs"),
        r#"/// Reads a byte of a buffer nothing initialized.
pub fn first_uninit(n: usize) -> u8 {
    let mut v: Vec<u8> = Vec::with_capacity(n);
    unsafe { v.set_len(n) };
    v[0]
}

/// Reads a u32 through a cast pointer: past the end of a short buffer,
/// misaligned on an odd address.
pub fn read_u32(data: &[u8]) -> u32 {
    if data.is_empty() {
        return 0;
    }
    unsafe { *(data.as_ptr() as *const u32) }
}

pub fn checked_sum(data: &[u8]) -> u32 {
    let mut total = 0u32;
    for chunk in data.chunks_exact(4) {
        total = total.wrapping_add(unsafe { std::ptr::read_unaligned(chunk.as_ptr() as *const u32) });
    }
    total
}

extern "C" {
    fn codegraph_fixture_missing() -> i32;
}

pub fn call_c() -> i32 {
    unsafe { codegraph_fixture_missing() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_first() {
        let _ = first_uninit(4);
    }

    #[test]
    fn sums() {
        assert_eq!(checked_sum(&[1, 0, 0, 0]), 1);
    }

    #[test]
    fn calls_c() {
        let _ = call_c();
    }
}
"#,
    );
}

fn miri_available() -> bool {
    std::process::Command::new("cargo")
        .args(["+nightly", "miri", "--version"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[test]
fn analyze_miri_plans_reaching_tests_and_harnesses_without_running() {
    let (_dir, root) = temp_project();
    write_miri_fixture(&root);
    init_fixture_files_only(&root);

    let plan = run_analyze_json(&root, &["miri", "--dry-run"]);
    let aimed: Vec<&str> = plan["aimed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["function"].as_str().unwrap())
        .collect();
    assert_eq!(aimed, ["first_uninit", "read_u32", "checked_sum", "call_c"], "{plan}");
    assert_eq!(plan["testsFound"], 3);
    let planned = plan["planned"].as_array().unwrap();
    let tests: Vec<(&str, &str)> = planned
        .iter()
        .map(|p| (p["kind"].as_str().unwrap(), p["test"].as_str().unwrap()))
        .collect();
    // The tests reaching each unsafe function, then the harness inputs for
    // the one no test reaches.
    assert_eq!(
        tests,
        [
            ("test", "tests::reads_first"),
            ("test", "tests::sums"),
            ("test", "tests::calls_c"),
            ("harness", "empty"),
            ("harness", "one_zero_byte"),
            ("harness", "one_high_byte"),
            ("harness", "unaligned_32"),
            ("harness", "pattern_64"),
        ],
        "{plan}"
    );
    assert_eq!(planned[0]["reaches"][0], "first_uninit");
    assert_eq!(planned[0]["target"]["kind"], "lib");
    let harness = &plan["harnesses"][0];
    assert_eq!(harness["function"], "read_u32");
    assert_eq!(harness["calls"], "ubdemo::read_u32");
    let source = harness["source"].as_str().unwrap();
    assert!(
        source.contains("#[test]\nfn unaligned_32() {\n    let storage = Aligned("),
        "{source}"
    );
    assert!(source.contains("    let _ = ubdemo::read_u32(data);\n}"), "{source}");
    assert!(plan["uncovered"].as_array().is_none_or(Vec::is_empty), "{plan}");
    // A dry run writes nothing.
    assert!(!root.join(".codegraph/miri").exists());

    // Aimed at one finding: only the test reaching it.
    let focused = run_analyze_json(&root, &["miri", "--dry-run", "--finding", "src/lib.rs:5"]);
    let planned = focused["planned"].as_array().unwrap();
    assert_eq!(planned.len(), 1, "{focused}");
    assert_eq!(planned[0]["test"], "tests::reads_first");

    // Without harnesses the unreached function is reported, not dropped.
    let bare = run_analyze_json(&root, &["miri", "--dry-run", "--function", "read_u32", "--no-harness"]);
    assert_eq!(bare["uncovered"][0]["function"], "read_u32", "{bare}");
    assert!(bare["uncovered"][0]["reason"].as_str().unwrap().contains("no test reaches it"));
}

#[test]
fn analyze_miri_runs_miri_and_maps_ub_back_to_the_graph() {
    if !miri_available() {
        eprintln!("skipping: `cargo +nightly miri` is not available");
        return;
    }
    let (_dir, root) = temp_project();
    write_miri_fixture(&root);
    init_fixture_files_only(&root);
    let target_dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("miri-fixture-target");

    let out = run_cli(
        &root,
        &[
            "analyze",
            "miri",
            "--json",
            "--target-dir",
            target_dir.to_str().unwrap(),
        ],
    );
    // Exit 4: Miri proved UB.
    assert_eq!(out.status.code(), Some(4), "{}", stderr_str(&out));
    let envelope: serde_json::Value = serde_json::from_str(stdout_str(&out).trim()).unwrap();
    let report = &envelope["data"];
    let runs = report["runs"].as_array().unwrap();
    let status_of = |test: &str| {
        runs.iter()
            .find(|r| r["test"] == test)
            .map(|r| r["status"].as_str().unwrap().to_string())
            .unwrap_or_else(|| panic!("no run {test}: {report}"))
    };
    assert_eq!(status_of("tests::reads_first"), "ub");
    assert_eq!(status_of("tests::sums"), "clean");
    // FFI is Miri's limit, not a clean pass.
    assert_eq!(status_of("tests::calls_c"), "unsupported");
    assert_eq!(status_of("empty"), "clean");
    assert_eq!(status_of("unaligned_32"), "ub");

    let findings = report["findings"].as_array().unwrap();
    let uninit = findings
        .iter()
        .find(|f| f["rule"] == "miri::uninit-read")
        .unwrap_or_else(|| panic!("{report}"));
    assert_eq!(uninit["detector"], "miri");
    assert_eq!(uninit["file"], "src/lib.rs");
    assert_eq!(uninit["line"], 5);
    assert_eq!(uninit["function"], "first_uninit");
    assert_eq!(uninit["confidence"], 1.0);
    assert!(
        uninit["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["note"].as_str().unwrap().contains("tests::reads_first")),
        "{uninit}"
    );
    let rules: Vec<&str> = findings.iter().map(|f| f["rule"].as_str().unwrap()).collect();
    assert!(rules.contains(&"miri::alignment"), "{rules:?}");
    assert!(rules.contains(&"miri::out-of-bounds"), "{rules:?}");
    let unsupported = runs.iter().find(|r| r["test"] == "tests::calls_c").unwrap();
    assert!(
        unsupported["reason"].as_str().unwrap().contains("foreign function"),
        "{unsupported}"
    );
    // The built-in `set_len` rule sits in the function Miri proved UB in.
    assert!(
        report["confirmed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["finding"]["rule"] == "rust-set-len-on-uninit" && c["by"] == "miri::uninit-read"),
        "{report}"
    );

    // Review shows the proof beside the static finding it confirms.
    let packets = run_analyze_json(&root, &["review", "--builtin", "--top", "20"]);
    let rule_packet = packets
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["finding"]["rule"] == "rust-set-len-on-uninit")
        .unwrap_or_else(|| panic!("{packets}"));
    assert_eq!(rule_packet["confirmedBy"][0]["rule"], "miri::uninit-read", "{rule_packet}");

    // A log mapped without running: the same finding.
    let log = root.join(".codegraph/miri/logs/ubdemo-lib-tests__reads_first.log");
    let out = run_cli(&root, &["analyze", "miri", "--json", "--log", log.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(4), "{}", stderr_str(&out));
    let mapped: serde_json::Value = serde_json::from_str(stdout_str(&out).trim()).unwrap();
    let mapped = &mapped["data"];
    assert_eq!(mapped["findings"][0]["rule"], "miri::uninit-read", "{mapped}");
    assert_eq!(mapped["findings"][0]["function"], "first_uninit");
}

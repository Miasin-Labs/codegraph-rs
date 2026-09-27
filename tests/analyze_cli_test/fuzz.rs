/// A small library crate: a byte decoder reaching an indexing helper, a
/// text parser an existing fuzz target already calls, a method on a type
/// with a `new`, and private/`pub(crate)` functions no fuzz target can call.
fn write_fuzz_fixture(root: &std::path::Path) {
    support::write(
        &root.join("Cargo.toml"),
        "[package]\nname = \"demo-codec\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    support::write(
        &root.join("src/lib.rs"),
        r#"pub mod text;
mod internal;

pub use internal::Decoder;

/// Decode a length-prefixed frame.
pub fn decode(input: &[u8]) -> Result<Vec<u8>, String> {
    if input.is_empty() {
        return Err("empty".to_string());
    }
    let len = checksum(input) as usize;
    Ok(input[1..len].to_vec())
}

fn checksum(data: &[u8]) -> u8 {
    data[0].wrapping_add(data[data.len() - 1])
}

pub fn scale(values: &[u16], width: u32) -> u32 {
    values.len() as u32 * width
}
"#,
    );
    support::write(
        &root.join("src/text.rs"),
        r#"pub fn parse(source: &str) -> Option<u32> {
    source.trim().parse().ok()
}

pub(crate) fn hidden(source: &str) -> usize {
    source.len()
}
"#,
    );
    support::write(
        &root.join("src/internal.rs"),
        r#"pub struct Decoder {
    state: usize,
}

impl Decoder {
    pub fn new() -> Self {
        Decoder { state: 0 }
    }

    pub fn feed(&mut self, chunk: &[u8]) -> usize {
        self.state += chunk[0] as usize;
        self.state
    }
}
"#,
    );
    support::write(
        &root.join("fuzz/fuzz_targets/parse_text.rs"),
        "#![no_main]\nuse libfuzzer_sys::fuzz_target;\n\nfuzz_target!(|data: &[u8]| {\n    \
         if let Ok(s) = std::str::from_utf8(data) {\n        let _ = demo_codec::text::parse(s);\n    \
         }\n});\n",
    );
}

#[test]
fn analyze_fuzz_targets_rank_and_harness_generates_a_cargo_fuzz_project() {
    let (_dir, root) = temp_project();
    write_fuzz_fixture(&root);
    init_fixture_files_only(&root);

    let report = run_analyze_json(&root, &["fuzz-targets"]);
    assert_eq!(report["ecosystem"], "rust");
    assert_eq!(report["crates"][0], "demo-codec");
    let targets = report["targets"].as_array().unwrap();
    let paths: Vec<&str> = targets.iter().map(|t| t["path"].as_str().unwrap()).collect();
    // The byte decoder ranks first; private, `pub(crate)` and already-fuzzed
    // functions are not targets.
    assert_eq!(paths[0], "demo_codec::decode", "{report}");
    assert_eq!(targets[0]["input"], "bytes");
    assert_eq!(targets[0]["harness"], "complete");
    assert!(targets[0]["features"]["parserName"].as_bool().unwrap());
    assert!(
        targets[0]["reaches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "checksum"),
        "{report}"
    );
    assert!(paths.contains(&"demo_codec::Decoder::feed"), "{paths:?}");
    assert!(paths.contains(&"demo_codec::scale"), "{paths:?}");
    for hidden in ["demo_codec::text::hidden", "demo_codec::checksum", "demo_codec::text::parse"] {
        assert!(!paths.contains(&hidden), "{hidden} in {paths:?}");
    }
    assert_eq!(report["existingTargets"][0], "parse_text");
    assert_eq!(report["alreadyFuzzed"], 1, "{report}");
    let feed = targets
        .iter()
        .find(|t| t["path"] == "demo_codec::Decoder::feed")
        .unwrap();
    assert_eq!(feed["receiver"]["constructor"], "Decoder::new");

    // The human form prints the same ranking.
    let out = run_cli(&root, &["analyze", "fuzz-targets"]);
    assert!(out.status.success(), "{}", stderr_str(&out));
    assert!(stdout_str(&out).contains("demo_codec::decode"), "{}", stdout_str(&out));

    // Aimed at the private helper: only what reaches it, nearest first.
    let focused = run_analyze_json(&root, &["fuzz-targets", "--finding", "src/lib.rs:16"]);
    assert_eq!(focused["focus"]["functions"][0], "checksum", "{focused}");
    let focused_targets = focused["targets"].as_array().unwrap();
    assert_eq!(focused_targets.len(), 1, "{focused}");
    assert_eq!(focused_targets[0]["path"], "demo_codec::decode");
    assert_eq!(focused_targets[0]["distance"], 1);

    // A harness for the private helper goes through the public decoder.
    let harness = run_analyze_json(&root, &["fuzz-harness", "--function", "checksum"]);
    assert_eq!(harness["path"], "demo_codec::decode", "{harness}");
    assert_eq!(harness["reaches"], "checksum");
    assert_eq!(harness["harness"], "complete");
    let target = std::fs::read_to_string(root.join("fuzz/fuzz_targets/decode.rs")).unwrap();
    assert!(target.starts_with("#![no_main]\n"), "{target}");
    assert!(target.contains("fuzz_target!(|data: &[u8]| {\n    let _ = demo_codec::decode(data);\n});"), "{target}");
    let manifest = std::fs::read_to_string(root.join("fuzz/Cargo.toml")).unwrap();
    assert!(manifest.contains("[dependencies.demo-codec]\npath = \"..\""), "{manifest}");
    assert!(manifest.contains("name = \"decode\""), "{manifest}");
    // Deterministic: generating again changes nothing.
    let again = run_analyze_json(&root, &["fuzz-harness", "--function", "demo_codec::decode"]);
    assert!(
        again["files"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["status"] == "unchanged"),
        "{again}"
    );

    // A method gets its receiver from `new`; scalars go through Arbitrary.
    let method = run_analyze_json(
        &root,
        &["fuzz-harness", "--function", "Decoder::feed", "--dry-run"],
    );
    let source = method["source"].as_str().unwrap();
    assert!(source.contains("let mut value = demo_codec::Decoder::new();"), "{source}");
    assert!(source.contains("let _ = value.feed(data);"), "{source}");
    let scale = run_analyze_json(&root, &["fuzz-harness", "--function", "scale", "--dry-run"]);
    let source = scale["source"].as_str().unwrap();
    assert!(source.contains("#[derive(arbitrary::Arbitrary, Debug)]"), "{source}");
    assert!(source.contains("demo_codec::scale(input.values.as_slice(), input.width % 1024)"), "{source}");

    // The generated target now counts as existing: decode is left out.
    let after = run_analyze_json(&root, &["fuzz-targets"]);
    assert_eq!(after["alreadyFuzzed"], 2, "{after}");
    let out = run_cli(&root, &["analyze", "fuzz-harness", "--function", "nope"]);
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("no indexed function named `nope`"), "{}", stderr_str(&out));
}

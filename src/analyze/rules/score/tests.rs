//! Scoring semantics, pinned against `tools/bugbench` on tiny fixtures: the
//! numbers here were produced by `score.py`'s own functions, and when a
//! `python3` is at hand the same fixtures are re-scored by them live.

use std::path::Path;

use serde_json::{Value, json};

use super::corpus::{CorpusKind, PairSide, enumerate_units};
use super::differential::{PairInput, region_rows, score_pairs};
use super::gt::{GtRow, parse_rows};
use super::labeled::score_labeled;
use super::pyrandom::PyRandom;
use super::{ScoredFinding, ratio};

/// Labeled rows: function rows (with flaw lines), a line row, a file row,
/// and classes by CWE, by category, and neither.
const LABELED_GT: &str = r#"
{"file": "a.c", "label": "bad", "granularity": "function", "line_start": 10, "line_end": 20, "function": "bad", "cwe": "CWE-121", "flaw_lines": [15]}
{"file": "a.c", "label": "good", "granularity": "function", "line_start": 30, "line_end": 40, "function": "good1", "cwe": "CWE-121"}
{"file": "a.c", "label": "good", "granularity": "function", "line_start": 50, "line_end": 60, "function": "goodG2B", "cwe": "CWE-121"}
{"file": "b.c", "label": "bad", "line_start": 5, "line_end": 5, "category": "sqli"}
{"file": "b.c", "label": "good", "granularity": "function", "line_start": 20, "line_end": 25, "category": "sqli"}
{"file": "c.java", "label": "bad", "granularity": "file", "cwe": "CWE-89"}
{"file": "d.py", "label": "bad", "granularity": "function", "line_start": 1, "line_end": 3, "cwe": ""}
{"file": "d.py", "label": "context_vuln", "granularity": "function", "line_start": 4, "line_end": 9}
"#;

fn finding(
    rule: &str,
    file: &str,
    line: i64,
    function: Option<&str>,
    message: &str,
) -> ScoredFinding {
    ScoredFinding {
        unit: "u".into(),
        rule_id: rule.into(),
        file: file.into(),
        local_file: file
            .rsplit_once("/vuln/")
            .or_else(|| file.rsplit_once("/fixed/"))
            .map_or(file, |(_, f)| f)
            .into(),
        line,
        function: function.map(str::to_string),
        message: message.into(),
        confidence: 0.5,
    }
}

fn labeled_findings() -> Vec<ScoredFinding> {
    vec![
        finding("rules:strcpy", "a.c", 15, Some("bad"), "m"), // tp, flaw line
        finding("rules:strcpy", "a.c", 12, Some("bad"), "m"), // tp
        finding("rules:strcpy", "a.c", 35, Some("good1"), "m"), // fp
        finding("rules:strcpy", "a.c", 70, None, "m"),        // unlabeled
        finding("rules:sql", "b.c", 7, None, "m"),            // tp: line row + slack 3
        finding("rules:sql", "b.c", 9, None, "m"),            // unlabeled: beyond slack
        finding("rules:sql", "b.c", 22, None, "m"),           // fp
        finding("rules:sql", "c.java", 400, None, "m"),       // tp: file row
        finding("rules:eval", "d.py", 2, None, "m"),          // tp, class "?"
        finding("rules:eval", "d.py", 5, None, "m"),          // unlabeled (other label)
        finding("rules:eval", "e.py", 1, None, "m"),          // unlabeled: no rows
    ]
}

/// What `labeled.score_labeled(findings, rows, 3)` returned for these
/// (verdicts popped).
fn labeled_expected() -> Value {
    json!({
        "rows": {"bad": 4, "good": 3, "other": 1},
        "base_rate": 0.5714,
        "overall": {"findings": 11, "tp": 5, "fp": 2, "unlabeled": 4, "flaw_line_hits": 1,
                    "precision": 0.7143, "bad_hit": 4, "recall": 1.0, "good_flagged": 2,
                    "good_flag_rate": 0.6667},
        "per_rule": {
            "rules:eval": {"findings": 3, "tp": 1, "fp": 0, "unlabeled": 2, "flaw_line_hits": 0,
                           "precision": 1.0, "bad_hit": 1, "recall": 0.25, "good_flagged": 0,
                           "good_flag_rate": 0.0},
            "rules:sql": {"findings": 4, "tp": 2, "fp": 1, "unlabeled": 1, "flaw_line_hits": 0,
                          "precision": 0.6667, "bad_hit": 2, "recall": 0.5, "good_flagged": 1,
                          "good_flag_rate": 0.3333},
            "rules:strcpy": {"findings": 4, "tp": 2, "fp": 1, "unlabeled": 1, "flaw_line_hits": 1,
                             "precision": 0.6667, "bad_hit": 1, "recall": 0.25, "good_flagged": 1,
                             "good_flag_rate": 0.3333}
        },
        "per_class": {
            "?": {"bad": 1, "good": 0, "bad_hit": 1, "recall": 1.0, "good_flagged": 0,
                  "precision": 1.0, "tp_by_rule": {"rules:eval": 1}, "fp_by_rule": {}},
            "CWE-121": {"bad": 1, "good": 2, "bad_hit": 1, "recall": 1.0, "good_flagged": 1,
                        "precision": 0.6667, "tp_by_rule": {"rules:strcpy": 2},
                        "fp_by_rule": {"rules:strcpy": 1}},
            "CWE-89": {"bad": 1, "good": 0, "bad_hit": 1, "recall": 1.0, "good_flagged": 0,
                       "precision": 1.0, "tp_by_rule": {"rules:sql": 1}, "fp_by_rule": {}},
            "sqli": {"bad": 1, "good": 1, "bad_hit": 1, "recall": 1.0, "good_flagged": 1,
                     "precision": 0.5, "tp_by_rule": {"rules:sql": 1},
                     "fp_by_rule": {"rules:sql": 1}}
        }
    })
}

/// Two pairs: one localized by `bad` rows, one only by `bad_candidate`.
const PAIRS_GT: &str = r#"
{"file": "RUSTSEC-1/vuln/src/lib.rs", "label": "bad", "granularity": "function", "line_start": 10, "line_end": 30, "function": "f"}
{"file": "RUSTSEC-1/vuln/src/lib.rs", "label": "bad", "granularity": "line", "line_start": 50, "line_end": 52}
{"file": "RUSTSEC-1/vuln/src/lib.rs", "label": "bad_candidate", "granularity": "line", "line_start": 90, "line_end": 90}
{"file": "RUSTSEC-1/fixed/src/lib.rs", "label": "good", "granularity": "function", "line_start": 10, "line_end": 31, "function": "f"}
{"file": "RUSTSEC-2/vuln/src/a.rs", "label": "bad_candidate", "granularity": "line", "line_start": 5, "line_end": 7}
{"file": "RUSTSEC-3/vuln/src/a.rs", "label": "context_vuln", "granularity": "line", "line_start": 5, "line_end": 7}
"#;

fn pair_findings() -> (Vec<ScoredFinding>, Vec<ScoredFinding>) {
    let v1 = "RUSTSEC-1/vuln/src/lib.rs";
    let f1 = "RUSTSEC-1/fixed/src/lib.rs";
    let v2 = "RUSTSEC-2/vuln/src/a.rs";
    let f2 = "RUSTSEC-2/fixed/src/a.rs";
    let v3 = "RUSTSEC-3/vuln/src/a.rs";
    let vuln = vec![
        finding("rules:a", v1, 12, Some("f"), "x 1"), // tp: gone in fixed
        finding("rules:a", v1, 20, Some("g"), "x 2"), // not_discriminating: fixed has (a, g)
        finding("rules:b", v1, 55, None, "at 55"),    // not_discriminating: fixed has "at #"
        finding("rules:b", v1, 70, None, "at 70"),    // background: the same key
        finding("rules:b", v1, 90, None, "bc 90"),    // vuln_only_elsewhere (candidate ignored)
        finding("rules:a", v2, 6, Some("h"), "y"),    // tp (candidate region)
        finding("rules:c", v3, 6, Some("k"), "z"),    // no region: vuln_only_elsewhere
    ];
    let fixed = vec![
        finding("rules:a", f1, 21, Some("g"), "x 3"),
        finding("rules:b", f1, 71, None, "at 71"),
        finding("rules:c", f1, 3, Some("new"), "introduced"),
        finding("rules:a", f2, 1, Some("h2"), "y"),
    ];
    (vuln, fixed)
}

/// What `differential.score_pairs(pairs, 5)` returned (verdicts popped).
const PAIRS_EXPECTED: &str = r#"{"pairs": 3, "scoreable_pairs": 2, "slack": 5, "overall": {"tp": 2, "not_discriminating": 2, "vuln_only_elsewhere": 2, "background": 1, "introduced": 2, "vuln_findings": 7, "pairs_detected": 2, "recall_pairs": 1.0, "precision_differential": 0.5, "discriminating_share_in_region": 0.5}, "per_rule": {"rules:a": {"tp": 2, "not_discriminating": 1, "vuln_only_elsewhere": 0, "background": 0, "introduced": 1, "vuln_findings": 3, "pairs_detected": 2, "recall_pairs": 1.0, "precision_differential": 1.0, "discriminating_share_in_region": 0.6667}, "rules:b": {"tp": 0, "not_discriminating": 1, "vuln_only_elsewhere": 1, "background": 1, "introduced": 0, "vuln_findings": 3, "pairs_detected": 0, "recall_pairs": 0.0, "precision_differential": 0.0, "discriminating_share_in_region": 0.0}, "rules:c": {"tp": 0, "not_discriminating": 0, "vuln_only_elsewhere": 1, "background": 0, "introduced": 1, "vuln_findings": 1, "pairs_detected": 0, "recall_pairs": 0.0, "precision_differential": 0.0, "discriminating_share_in_region": null}}, "per_category": {"(none)": {"pairs": 1, "scoreable": 0, "detected": 0, "region_findings": 0, "recall": null}, "informational:unsound": {"pairs": 1, "scoreable": 1, "detected": 1, "region_findings": 3, "recall": 1.0}, "memory-corruption": {"pairs": 2, "scoreable": 2, "detected": 2, "region_findings": 4, "recall": 1.0}}, "per_pair": [{"advisory": "RUSTSEC-1", "categories": ["memory-corruption", "informational:unsound"], "localization": "fix_commit", "region_rows": 2, "vuln_findings": 5, "fixed_findings": 3, "tp": 1, "not_discriminating": 2, "vuln_only_elsewhere": 1, "background": 1, "introduced": 1}, {"advisory": "RUSTSEC-2", "categories": ["memory-corruption"], "localization": "tight_diff", "region_rows": 1, "vuln_findings": 1, "fixed_findings": 1, "tp": 1, "not_discriminating": 0, "vuln_only_elsewhere": 0, "background": 0, "introduced": 1}, {"advisory": "RUSTSEC-3", "categories": ["(none)"], "localization": "none", "region_rows": 0, "vuln_findings": 1, "fixed_findings": 0, "tp": 0, "not_discriminating": 0, "vuln_only_elsewhere": 1, "background": 0, "introduced": 0}]}"#;

/// The same, as a value.
fn pairs_expected() -> Value {
    serde_json::from_str(PAIRS_EXPECTED).unwrap()
}

/// The pairs, as `score.py`'s `rustsec_scorer` builds them.
fn pairs<'a>(
    rows: &[GtRow],
    vuln: &'a [ScoredFinding],
    fixed: &'a [ScoredFinding],
) -> Vec<PairInput<'a>> {
    let meta = [
        (
            "RUSTSEC-1",
            vec!["memory-corruption", "informational:unsound"],
            "fix_commit",
        ),
        ("RUSTSEC-2", vec!["memory-corruption"], "tight_diff"),
        ("RUSTSEC-3", vec!["(none)"], "none"),
    ];
    meta.iter()
        .map(|(id, categories, localization)| {
            let of = |findings: &'a [ScoredFinding]| {
                findings
                    .iter()
                    .filter(|f| f.file.starts_with(&format!("{id}/")))
                    .collect::<Vec<_>>()
            };
            let advisory_rows: Vec<&GtRow> = rows
                .iter()
                .filter(|r| r.file.split('/').next() == Some(*id))
                .collect();
            PairInput {
                advisory: id.to_string(),
                categories: categories.iter().map(|c| c.to_string()).collect(),
                localization: localization.to_string(),
                vuln: of(vuln),
                fixed: of(fixed),
                region: region_rows(id, &advisory_rows),
            }
        })
        .collect()
}

fn rust_labeled() -> Value {
    let rows = parse_rows(LABELED_GT, None).unwrap();
    let findings = labeled_findings();
    serde_json::to_value(score_labeled(&findings, &rows, 3).0).unwrap()
}

fn rust_pairs() -> Value {
    let rows = parse_rows(PAIRS_GT, None).unwrap();
    let (vuln, fixed) = pair_findings();
    serde_json::to_value(score_pairs(&pairs(&rows, &vuln, &fixed), 5).0).unwrap()
}

#[test]
fn labeled_scoring_matches_score_py() {
    assert_eq!(rust_labeled(), labeled_expected());
}

#[test]
fn differential_scoring_matches_score_py() {
    assert_eq!(rust_pairs(), pairs_expected());
}

/// Re-score the fixtures with bugbench's own Python and compare, when a
/// `python3` is available.
#[test]
fn fixtures_rescored_by_bugbench_python_agree() {
    let bench = Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/bugbench");
    if !bench.join("score.py").is_file() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let labeled_gt = dir.path().join("labeled.jsonl");
    let pairs_gt = dir.path().join("pairs.jsonl");
    std::fs::write(&labeled_gt, LABELED_GT).unwrap();
    std::fs::write(&pairs_gt, PAIRS_GT).unwrap();
    let as_py = |findings: &[ScoredFinding]| -> Value {
        findings
            .iter()
            .map(|f| {
                json!([
                    f.rule_id.trim_start_matches("rules:"),
                    f.file,
                    f.local_file,
                    f.line,
                    f.function,
                    f.message
                ])
            })
            .collect()
    };
    let (vuln, fixed) = pair_findings();
    let input = json!({
        "labeled_gt": labeled_gt, "pairs_gt": pairs_gt,
        "labeled": as_py(&labeled_findings()), "vuln": as_py(&vuln), "fixed": as_py(&fixed),
    });
    let script = r#"
import json, sys
sys.path.insert(0, sys.argv[1])
from findings import Finding
from gt import load_rows
from labeled import score_labeled
from differential import PairInput, region_rows, score_pairs
data = json.load(sys.stdin)
mk = lambda r: Finding("u", "rules", r[0], r[1], r[2], r[3], r[4], r[5], 0.5, "rule")
labeled = score_labeled([mk(r) for r in data["labeled"]], load_rows(data["labeled_gt"]), 3)
labeled.pop("verdicts")
rows = load_rows(data["pairs_gt"])
meta = [("RUSTSEC-1", ["memory-corruption", "informational:unsound"], "fix_commit"),
        ("RUSTSEC-2", ["memory-corruption"], "tight_diff"), ("RUSTSEC-3", ["(none)"], "none")]
vuln = [mk(r) for r in data["vuln"]]; fixed = [mk(r) for r in data["fixed"]]
pairs = [PairInput(i, c, loc, [f for f in vuln if f.file.startswith(i + "/")],
                   [f for f in fixed if f.file.startswith(i + "/")],
                   region_rows(i, [r for r in rows if r.file.split("/", 1)[0] == i]))
         for i, c, loc in meta]
diff = score_pairs(pairs, 5)
diff.pop("verdicts")
print(json.dumps({"labeled": labeled, "pairs": diff}))
"#;
    let child = std::process::Command::new("python3")
        .arg("-c")
        .arg(script)
        .arg(&bench)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();
    let Ok(mut child) = child else {
        return; // no python3
    };
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "bugbench python failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let python: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(python["labeled"], rust_labeled());
    assert_eq!(python["pairs"], rust_pairs());
}

#[test]
fn ratios_round_like_python() {
    assert_eq!(ratio(1, 3), Some(0.3333));
    assert_eq!(ratio(2, 3), Some(0.6667));
    // An exact binary tie rounds half to even, as Python's round() does.
    assert_eq!(ratio(1, 32), Some(0.0312));
    assert_eq!(ratio(3, 32), Some(0.0938));
    assert_eq!(ratio(1, 0), None);
}

#[test]
fn rows_default_their_granularity_like_gt_py() {
    let rows = parse_rows(
        r#"{"file": "x", "label": "bad", "line_start": 3}
{"file": "y", "label": "bad"}
{"file": "z", "label": null, "line_start": 0}"#,
        None,
    )
    .unwrap();
    assert_eq!(rows[0].granularity, "line");
    assert!(rows[0].covers(6, 3) && !rows[0].covers(7, 3));
    assert_eq!(rows[1].granularity, "file");
    assert_eq!(rows[2].granularity, "file");
    assert_eq!(rows[2].label, "None");
    assert!(rows[0].overlaps(7, 9, 4) && !rows[0].overlaps(8, 9, 4));
}

/// `random.Random(1).sample(range(n), k)` in CPython 3.
#[test]
fn sampling_matches_cpython() {
    // python3 -c 'import random; r = random.Random(1); print(r.sample(range(100), 5), r.sample(range(1000), 40)[:5], r.sample(range(10), 10))'
    let mut rng = PyRandom::new(1);
    assert_eq!(rng.sample_indices(100, 5), vec![17, 72, 97, 8, 32]);
    assert_eq!(
        &rng.sample_indices(1000, 40)[..5],
        &[120, 507, 779, 460, 483]
    );
    let mut all = rng.sample_indices(10, 10);
    all.sort();
    assert_eq!(all, (0..10).collect::<Vec<_>>());
}

#[test]
fn corpora_are_split_into_bugbench_units() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("advisories.jsonl"),
        [
            r#"{"advisory": "RUSTSEC-B", "vuln_dir": "RUSTSEC-B/vuln", "fixed_dir": "RUSTSEC-B/fixed", "diff_tightness": "tight", "localization": "fix_commit"}"#,
            r#"{"advisory": "RUSTSEC-A", "vuln_dir": "RUSTSEC-A/vuln", "fixed_dir": "RUSTSEC-A/fixed", "diff_tightness": "tight", "localization": "name_match"}"#,
            r#"{"advisory": "RUSTSEC-C", "vuln_dir": "RUSTSEC-C/vuln", "fixed_dir": "RUSTSEC-C/fixed", "diff_tightness": "loose", "localization": "fix_commit"}"#,
        ]
        .join("\n"),
    )
    .unwrap();
    assert_eq!(CorpusKind::detect(root), CorpusKind::Pairs);
    let units = enumerate_units(root, CorpusKind::Pairs, None, 1).unwrap();
    let names: Vec<&str> = units.iter().map(|u| u.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "RUSTSEC-A/vuln",
            "RUSTSEC-A/fixed",
            "RUSTSEC-B/vuln",
            "RUSTSEC-B/fixed"
        ]
    );
    assert_eq!(units[0].prefix, "RUSTSEC-A/vuln/");
    assert_eq!(
        units[1].pair,
        Some(("RUSTSEC-A".to_string(), PairSide::Fixed))
    );
    assert_eq!(
        enumerate_units(root, CorpusKind::Pairs, Some(1), 1)
            .unwrap()
            .len(),
        2
    );

    let whole = tempfile::tempdir().unwrap();
    assert_eq!(CorpusKind::detect(whole.path()), CorpusKind::Whole);
    let units = enumerate_units(whole.path(), CorpusKind::Whole, Some(3), 1).unwrap();
    assert_eq!(units[0].sources, ["."]);
}

#[test]
fn juliet_units_sample_testcases_per_cwe() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let cwe = root.join("testcases/CWE476_NULL_Pointer_Dereference");
    std::fs::create_dir_all(&cwe).unwrap();
    std::fs::create_dir_all(root.join("testcases/CWE999_Other")).unwrap();
    std::fs::create_dir_all(root.join("testcasesupport")).unwrap();
    for name in ["io.c", "std_testcase.h", "main.cpp"] {
        std::fs::write(root.join("testcasesupport").join(name), "").unwrap();
    }
    for id in 1..=9 {
        std::fs::write(cwe.join(format!("CWE476_NULL__int_{id:02}.c")), "").unwrap();
        std::fs::write(cwe.join(format!("CWE476_NULL__int_{id:02}a.c")), "").unwrap();
    }
    std::fs::write(root.join("testcases/CWE999_Other/CWE999_x_01.c"), "").unwrap();
    assert_eq!(CorpusKind::detect(root), CorpusKind::JulietC);
    let units = enumerate_units(root, CorpusKind::JulietC, Some(3), 1).unwrap();
    assert_eq!(units.len(), 1, "only the sampled CWEs");
    let unit = &units[0];
    assert_eq!(unit.name, "CWE476_NULL_Pointer_Dereference");
    // 3 testcases of 2 files each, then the listed support files.
    assert_eq!(unit.sources.len(), 8, "{:?}", unit.sources);
    assert!(unit.sources.ends_with(&[
        "testcasesupport/io.c".to_string(),
        "testcasesupport/std_testcase.h".to_string()
    ]));
    // random.Random(1).sample(ids, 3) over the 9 sorted ids → ids 02, 03, 09
    // (python3 -c 'import random; print(sorted(random.Random(1).sample(range(9), 3)))').
    assert_eq!(
        unit.sources[0],
        "testcases/CWE476_NULL_Pointer_Dereference/CWE476_NULL__int_02.c"
    );
}

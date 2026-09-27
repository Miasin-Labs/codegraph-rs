//! What gets indexed: a corpus split into units, each a directory to stage
//! — `tools/bugbench/units.py`, unit for unit (same sampling, same seed).
//!
//! A unit is staged as a copy, laid out so that a finding's project-relative
//! path plus the unit's `prefix` is the path the corpus ground truth uses.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::pyrandom::PyRandom;

/// Juliet testcase id: `<CWE..>_NN` before the optional `a`..`z` part and role.
static JULIET_ID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(.*_\d\d)[a-z]?(?:_[A-Za-z]\w*)?\.(c|cpp|h|java)$").expect("valid regex")
});

/// CWEs sampled from Juliet C/C++ (bugbench's list).
const JULIET_C_CWES: &[&str] = &[
    "CWE121", "CWE122", "CWE134", "CWE190", "CWE252", "CWE253", "CWE369", "CWE398", "CWE401",
    "CWE415", "CWE416", "CWE476", "CWE480", "CWE481", "CWE482", "CWE483", "CWE484", "CWE563",
    "CWE570", "CWE571", "CWE78", "CWE835",
];
/// CWEs sampled from Juliet Java.
const JULIET_JAVA_CWES: &[&str] = &[
    "CWE129", "CWE190", "CWE252", "CWE253", "CWE369", "CWE396", "CWE398", "CWE476", "CWE478",
    "CWE481", "CWE482", "CWE483", "CWE484", "CWE561", "CWE563", "CWE570", "CWE571", "CWE690",
    "CWE78", "CWE80", "CWE835", "CWE89",
];
/// The Juliet C support files every testcase includes (the generated
/// `main.cpp`/`testcases.h` are tens of MB and carry no flaws).
const JULIET_C_SUPPORT: &[&str] = &[
    "io.c",
    "std_testcase.h",
    "std_testcase_io.h",
    "std_thread.c",
    "std_thread.h",
];
/// RustSec pairs taken: small diffs with a located fix.
const RUSTSEC_TIGHTNESS: &[&str] = &["tight"];
const RUSTSEC_LOCALIZED: &[&str] = &["fix_commit", "rudra", "name_match", "tight_diff"];
/// Juliet testcases per CWE when no sample is given.
const JULIET_DEFAULT_SAMPLE: usize = 40;

/// How a corpus is laid out and scored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorpusKind {
    JulietC,
    JulietJava,
    /// Vulnerable/fixed pairs (`advisories.jsonl`), scored differentially.
    Pairs,
    /// One unit: the whole directory (OWASP Benchmark, the web apps).
    Whole,
}

impl CorpusKind {
    /// Read from the directory's layout.
    pub fn detect(dir: &Path) -> CorpusKind {
        if dir.join("advisories.jsonl").is_file() {
            CorpusKind::Pairs
        } else if dir.join("testcases").is_dir() && dir.join("testcasesupport").is_dir() {
            CorpusKind::JulietC
        } else if dir.join("src/testcases").is_dir() && dir.join("src/testcasesupport").is_dir() {
            CorpusKind::JulietJava
        } else {
            CorpusKind::Whole
        }
    }

    pub fn is_differential(self) -> bool {
        self == CorpusKind::Pairs
    }

    pub fn as_str(self) -> &'static str {
        match self {
            CorpusKind::JulietC => "juliet-c",
            CorpusKind::JulietJava => "juliet-java",
            CorpusKind::Pairs => "pairs",
            CorpusKind::Whole => "whole",
        }
    }
}

/// One directory to stage and index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    /// Unique within the corpus.
    pub name: String,
    /// Prepended to a finding's path to get the ground-truth path.
    pub prefix: String,
    /// Corpus-relative sources to copy (files, or one directory; `.` is the
    /// whole corpus).
    pub sources: Vec<String>,
    /// For pairs: the advisory and side (`vuln`/`fixed`).
    pub pair: Option<(String, PairSide)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairSide {
    Vuln,
    Fixed,
}

/// The units of the corpus at `dir`. `sample`: Juliet testcases per CWE
/// (default 40), or pairs (default all); ignored for a whole-directory
/// corpus.
pub fn enumerate_units(
    dir: &Path,
    kind: CorpusKind,
    sample: Option<usize>,
    seed: u64,
) -> Result<Vec<Unit>, String> {
    match kind {
        CorpusKind::JulietC | CorpusKind::JulietJava => {
            juliet_units(dir, kind, sample.unwrap_or(JULIET_DEFAULT_SAMPLE), seed)
        }
        CorpusKind::Pairs => pair_units(dir, sample, seed),
        CorpusKind::Whole => Ok(vec![Unit {
            name: "all".to_string(),
            prefix: String::new(),
            sources: vec![".".to_string()],
            pair: None,
        }]),
    }
}

/// Entries of `dir`, sorted by name.
fn sorted_entries(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot list {}: {e}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    entries.sort();
    Ok(entries)
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// One unit per sampled CWE: up to `per_cwe` whole testcases plus support.
fn juliet_units(
    root: &Path,
    kind: CorpusKind,
    per_cwe: usize,
    seed: u64,
) -> Result<Vec<Unit>, String> {
    let (testcases, support_dir, wanted, support_names): (&str, &str, &[&str], Option<&[&str]>) =
        match kind {
            CorpusKind::JulietC => (
                "testcases",
                "testcasesupport",
                JULIET_C_CWES,
                Some(JULIET_C_SUPPORT),
            ),
            _ => (
                "src/testcases",
                "src/testcasesupport",
                JULIET_JAVA_CWES,
                None,
            ),
        };
    let mut support: Vec<String> = sorted_entries(&root.join(support_dir))?
        .into_iter()
        .filter(|path| path.is_file())
        .filter(|path| {
            support_names.is_none_or(|names| {
                path.file_name()
                    .is_some_and(|name| names.contains(&name.to_string_lossy().as_ref()))
            })
        })
        .map(|path| relative(root, &path))
        .collect();
    support.sort();

    let mut rng = PyRandom::new(seed);
    let mut units = Vec::new();
    for cwe_dir in sorted_entries(&root.join(testcases))? {
        let name = cwe_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let cwe = name.split('_').next().unwrap_or("");
        if !cwe_dir.is_dir() || !wanted.contains(&cwe) {
            continue;
        }
        // id → files, in path order.
        let mut by_id: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        for entry in walkdir::WalkDir::new(&cwe_dir)
            .min_depth(1)
            .sort_by_file_name()
            .into_iter()
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let file_name = entry.file_name().to_string_lossy();
            if let Some(found) = JULIET_ID.captures(&file_name) {
                by_id
                    .entry(found[1].to_string())
                    .or_default()
                    .push(relative(root, entry.path()));
            }
        }
        let ids: Vec<&String> = by_id.keys().collect();
        let mut chosen: Vec<&String> = rng
            .sample_indices(ids.len(), per_cwe.min(ids.len()))
            .into_iter()
            .map(|i| ids[i])
            .collect();
        chosen.sort();
        let mut sources: Vec<String> = chosen
            .iter()
            .flat_map(|id| by_id[*id].iter().cloned())
            .collect();
        sources.extend(support.iter().cloned());
        units.push(Unit {
            name,
            prefix: String::new(),
            sources,
            pair: None,
        });
    }
    Ok(units)
}

/// A pair's advisory row.
#[derive(Debug, Clone)]
pub struct Advisory {
    pub id: String,
    pub vuln_dir: String,
    pub fixed_dir: String,
    pub categories: Vec<String>,
    pub localization: String,
    pub unsound: bool,
    tightness: String,
}

fn py_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "None".to_string(),
        other => other.to_string(),
    }
}

/// Every advisory of a pairs corpus.
pub fn load_advisories(dir: &Path) -> Result<Vec<Advisory>, String> {
    let path = dir.join("advisories.jsonl");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut out = Vec::new();
    for (number, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let raw: Value = serde_json::from_str(line)
            .map_err(|e| format!("{}:{}: {e}", path.display(), number + 1))?;
        let field = |key: &str| raw.get(key).map(py_str).unwrap_or_default();
        out.push(Advisory {
            id: field("advisory"),
            vuln_dir: field("vuln_dir"),
            fixed_dir: field("fixed_dir"),
            categories: raw
                .get("categories")
                .and_then(Value::as_array)
                .map(|list| list.iter().map(py_str).collect())
                .unwrap_or_default(),
            localization: field("localization"),
            unsound: raw.get("informational").and_then(Value::as_str) == Some("unsound"),
            tightness: field("diff_tightness"),
        });
    }
    Ok(out)
}

/// Each selected pair gives two units, `<id>/vuln` and `<id>/fixed`.
fn pair_units(dir: &Path, sample: Option<usize>, seed: u64) -> Result<Vec<Unit>, String> {
    let mut pairs: Vec<Advisory> = load_advisories(dir)?
        .into_iter()
        .filter(|a| {
            RUSTSEC_TIGHTNESS.contains(&a.tightness.as_str())
                && RUSTSEC_LOCALIZED.contains(&a.localization.as_str())
        })
        .collect();
    pairs.sort_by(|a, b| a.id.cmp(&b.id));
    if let Some(sample) = sample.filter(|s| *s > 0 && *s < pairs.len()) {
        let picked = PyRandom::new(seed).sample_indices(pairs.len(), sample);
        let mut chosen: Vec<Advisory> = picked.into_iter().map(|i| pairs[i].clone()).collect();
        chosen.sort_by(|a, b| a.id.cmp(&b.id));
        pairs = chosen;
    }
    let mut units = Vec::new();
    for advisory in pairs {
        for (side, rel) in [
            (PairSide::Vuln, &advisory.vuln_dir),
            (PairSide::Fixed, &advisory.fixed_dir),
        ] {
            units.push(Unit {
                name: rel.clone(),
                prefix: format!("{rel}/"),
                sources: vec![rel.clone()],
                pair: Some((advisory.id.clone(), side)),
            });
        }
    }
    Ok(units)
}

//! Codegraph's findings as a SARIF 2.1.0 log: one run, tool `codegraph`,
//! a rule descriptor per rule id (its description, tags, CWEs), and per
//! finding a result with its location (`%SRCROOT%`-relative), the enclosing
//! function as a logical location, the evidence as a code flow, the
//! confidence as `rank` and `level`, and a fingerprint that survives the
//! lines moving (rule + file + function + the flagged line's text).

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::analyze::bugs::{Detector, Finding};

/// What a rule descriptor says; [`RuleDoc::fallback`] when the caller has
/// nothing better.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RuleDoc {
    pub id: String,
    /// One line.
    pub short: String,
    pub full: Option<String>,
    pub help_uri: Option<String>,
    /// Tags as the rule states them (`CWE-89`, `security`, `taint`…).
    pub tags: Vec<String>,
    /// CodeQL's `precision` (`high`…), kept when re-exporting its results.
    pub precision: Option<String>,
    /// 0.0–10.0, GitHub's severity bands.
    pub security_severity: Option<f64>,
}

impl RuleDoc {
    /// A descriptor from the rule id and one of its findings.
    pub fn fallback(finding: &Finding) -> Self {
        let family = match finding.detector {
            Detector::Deviance => "Deviance: a departure from what the rest of the code does",
            Detector::Lint => "Lint: a syntactic bug shape",
            Detector::Rule => "Rule: a codegraph YAML rule",
            Detector::Compiler => "Compiler: a rustc/clippy lint",
            Detector::Miri => "Miri: undefined behaviour on a real execution",
            Detector::CodeQL => "CodeQL: a CodeQL query result",
        };
        Self {
            id: finding.rule.to_string(),
            short: format!("{} ({family})", finding.rule),
            ..Self::default()
        }
    }
}

/// The emitting tool.
#[derive(Debug, Clone)]
pub struct ToolInfo {
    pub name: String,
    pub version: String,
    pub information_uri: Option<String>,
}

impl Default for ToolInfo {
    fn default() -> Self {
        Self {
            name: "codegraph".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            information_uri: None,
        }
    }
}

/// SARIF level for a confidence: `error` ≥ 0.7, `warning` ≥ 0.4, else
/// `note`.
pub fn level_for(confidence: f64) -> &'static str {
    if confidence >= 0.7 {
        "error"
    } else if confidence >= 0.4 {
        "warning"
    } else {
        "note"
    }
}

/// `findings` (paths relative to `root`) as a SARIF log. `docs` describes
/// rules by id; a rule it lacks gets [`RuleDoc::fallback`]. Source lines
/// are read from `root` for the fingerprints (a file that cannot be read
/// fingerprints on the line number instead).
pub fn to_sarif(
    findings: &[Finding],
    root: &Path,
    docs: &BTreeMap<String, RuleDoc>,
    tool: &ToolInfo,
) -> Value {
    // Rules in first-seen order, so the log is stable for a stable input.
    let mut rule_index: HashMap<&str, usize> = HashMap::new();
    let mut rules: Vec<Value> = Vec::new();
    for finding in findings {
        let id = finding.rule.as_ref();
        if rule_index.contains_key(id) {
            continue;
        }
        rule_index.insert(id, rules.len());
        let doc = docs
            .get(id)
            .cloned()
            .unwrap_or_else(|| RuleDoc::fallback(finding));
        rules.push(rule_descriptor(&doc, finding.detector));
    }

    let mut sources: HashMap<&str, Option<Vec<String>>> = HashMap::new();
    let mut seen: HashMap<String, u32> = HashMap::new();
    let results: Vec<Value> = findings
        .iter()
        .map(|finding| {
            let lines = sources.entry(finding.file.as_str()).or_insert_with(|| {
                std::fs::read_to_string(root.join(&finding.file))
                    .ok()
                    .map(|text| text.lines().map(|l| l.trim().to_string()).collect())
            });
            let line_text = lines
                .as_ref()
                .and_then(|lines| lines.get(finding.line.saturating_sub(1) as usize))
                .cloned()
                .unwrap_or_else(|| format!("#{}", finding.line));
            let base = fingerprint(finding, &line_text);
            let n = seen.entry(base.clone()).or_default();
            *n += 1;
            let print = format!("{base}:{n}");
            result(finding, rule_index[finding.rule.as_ref()], &print)
        })
        .collect();

    let mut driver = Map::new();
    driver.insert("name".into(), json!(tool.name));
    driver.insert("semanticVersion".into(), json!(tool.version));
    if let Some(uri) = &tool.information_uri {
        driver.insert("informationUri".into(), json!(uri));
    }
    driver.insert("rules".into(), Value::Array(rules));

    json!({
        "$schema": super::SCHEMA_URI,
        "version": super::VERSION,
        "runs": [{
            "tool": {"driver": Value::Object(driver)},
            "originalUriBaseIds": {
                "%SRCROOT%": {"uri": dir_uri(root)}
            },
            "columnKind": "unicodeCodePoints",
            "results": results,
        }]
    })
}

fn rule_descriptor(doc: &RuleDoc, detector: Detector) -> Value {
    let mut descriptor = Map::new();
    descriptor.insert("id".into(), json!(doc.id));
    descriptor.insert("name".into(), json!(doc.id));
    descriptor.insert("shortDescription".into(), json!({"text": doc.short}));
    if let Some(full) = doc.full.as_deref().filter(|f| !f.is_empty()) {
        descriptor.insert("fullDescription".into(), json!({"text": full}));
        descriptor.insert("help".into(), json!({"text": full}));
    }
    if let Some(uri) = &doc.help_uri {
        descriptor.insert("helpUri".into(), json!(uri));
    }
    let mut props = Map::new();
    let mut tags = doc.tags.clone();
    // GitHub reads CWEs from `external/cwe/cwe-NNN` tags.
    for cwe in super::cwes_of_tags(doc.tags.iter().map(String::as_str)) {
        let tag = format!(
            "external/cwe/cwe-{:03}",
            cwe.trim_start_matches("CWE-")
                .parse::<u32>()
                .unwrap_or_default()
        );
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    if !tags.is_empty() {
        props.insert("tags".into(), json!(tags));
    }
    if let Some(precision) = &doc.precision {
        props.insert("precision".into(), json!(precision));
    }
    if let Some(severity) = doc.security_severity {
        props.insert("security-severity".into(), json!(format!("{severity:.1}")));
    }
    props.insert("detector".into(), json!(detector));
    descriptor.insert("properties".into(), Value::Object(props));
    Value::Object(descriptor)
}

fn physical(file: &str, line: u32, col: Option<u32>) -> Value {
    let mut location = Map::new();
    location.insert(
        "artifactLocation".into(),
        json!({"uri": encode_path(file), "uriBaseId": "%SRCROOT%"}),
    );
    if line > 0 {
        let mut region = Map::new();
        region.insert("startLine".into(), json!(line));
        if let Some(col) = col {
            region.insert("startColumn".into(), json!(col + 1));
        }
        location.insert("region".into(), Value::Object(region));
    }
    Value::Object(location)
}

fn result(finding: &Finding, rule_index: usize, print: &str) -> Value {
    let mut location = Map::new();
    location.insert(
        "physicalLocation".into(),
        physical(&finding.file, finding.line, Some(finding.col)),
    );
    if let Some(function) = &finding.function {
        location.insert(
            "logicalLocations".into(),
            json!([{"fullyQualifiedName": function, "kind": "function"}]),
        );
    }
    let mut result = Map::new();
    result.insert("ruleId".into(), json!(finding.rule));
    result.insert("ruleIndex".into(), json!(rule_index));
    result.insert("level".into(), json!(level_for(finding.confidence)));
    result.insert(
        "message".into(),
        json!({"text": if finding.message.is_empty() { finding.rule.to_string() } else { finding.message.clone() }}),
    );
    result.insert("locations".into(), json!([Value::Object(location)]));
    // SARIF's rank is 0–100 (higher = more important).
    let rank = (finding.confidence.clamp(0.0, 1.0) * 1000.0).round() / 10.0;
    result.insert("rank".into(), json!(rank));
    result.insert(
        "partialFingerprints".into(),
        json!({"codegraphFingerprint/v1": print}),
    );
    if !finding.evidence.is_empty() {
        let steps: Vec<Value> = finding
            .evidence
            .iter()
            .map(|step| {
                json!({
                    "location": {
                        "physicalLocation": physical(&step.file, step.line, None),
                        "message": {"text": step.note},
                    }
                })
            })
            .collect();
        result.insert(
            "codeFlows".into(),
            json!([{"threadFlows": [{"locations": steps}]}]),
        );
    }
    let mut props = Map::new();
    props.insert("confidence".into(), json!(finding.confidence));
    props.insert("detector".into(), json!(finding.detector));
    result.insert("properties".into(), Value::Object(props));
    Value::Object(result)
}

/// Rule, file, function and the flagged line's trimmed text, hashed: moving
/// the code up or down keeps the key.
fn fingerprint(finding: &Finding, line_text: &str) -> String {
    let mut hasher = Sha256::new();
    for part in [
        finding.rule.as_ref(),
        finding.file.as_str(),
        finding.function.as_deref().unwrap_or(""),
        line_text,
    ] {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    let digest = hasher.finalize();
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// A relative path as a URI reference: `/`-separated, anything but
/// unreserved characters and `/` percent-encoded.
fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.replace('\\', "/").bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `root` as a `file:` URI ending in `/`.
fn dir_uri(root: &Path) -> String {
    let path = root.to_string_lossy().replace('\\', "/");
    let path = if path.starts_with('/') {
        path
    } else {
        format!("/{path}")
    };
    let mut uri = format!("file://{}", encode_path(&path));
    if !uri.ends_with('/') {
        uri.push('/');
    }
    uri
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_encode_as_uri_references() {
        assert_eq!(encode_path("src/a b/ü.rs"), "src/a%20b/%C3%BC.rs");
        assert_eq!(dir_uri(Path::new("/w/p q")), "file:///w/p%20q/");
    }

    #[test]
    fn levels_follow_confidence() {
        assert_eq!(level_for(0.9), "error");
        assert_eq!(level_for(0.5), "warning");
        assert_eq!(level_for(0.1), "note");
    }
}

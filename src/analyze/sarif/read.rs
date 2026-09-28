//! Reading a SARIF 2.1.0 log into plain records: each result's rule and
//! message, its primary location, its code flows (a taint path's hops) and
//! related locations, and the rule metadata a consumer ranks by (CodeQL's
//! `precision`, `security-severity`, `problem.severity`, CWE tags).
//!
//! Tolerant: anything malformed or missing is skipped, never an error,
//! except a log that is not SARIF at all.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

/// One `run` of a log.
#[derive(Debug, Clone, Default)]
pub struct SarifRun {
    /// `tool.driver.name`.
    pub tool: String,
    /// `tool.driver.semanticVersion` (or `version`).
    pub tool_version: Option<String>,
    /// Every rule the driver and its extensions describe, by id.
    pub rules: BTreeMap<String, RuleMeta>,
    pub results: Vec<SarifResult>,
    /// `toolExecutionNotifications` / `toolConfigurationNotifications` at
    /// level `error`: what the tool said went wrong.
    pub errors: Vec<String>,
}

/// What a rule's descriptor says about it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleMeta {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short_description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help_uri: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// CodeQL: `very-high` | `high` | `medium` | `low`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub precision: Option<String>,
    /// CodeQL: `error` | `warning` | `recommendation`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem_severity: Option<String>,
    /// CVSS-like 0.0–10.0; present on security queries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security_severity: Option<f64>,
    /// `problem` | `path-problem` (CodeQL's `kind`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// `defaultConfiguration.level`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
}

impl RuleMeta {
    /// The CWEs the rule's tags name (`CWE-89`).
    pub fn cwes(&self) -> Vec<String> {
        super::cwes_of_tags(self.tags.iter().map(String::as_str))
    }

    /// Whether the rule is a security rule: it carries a
    /// `security-severity` or a `security` tag.
    pub fn is_security(&self) -> bool {
        self.security_severity.is_some() || self.tags.iter().any(|tag| tag == "security")
    }
}

/// A place in a file as SARIF gives it. Lines and columns are 1-based;
/// `0` means the log did not say.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SarifLocation {
    /// The artifact URI, percent-decoded (`src/A.java`, or
    /// `file:///abs/src/A.java`).
    pub uri: String,
    /// `%SRCROOT%` and the like, when the URI is relative to one.
    pub uri_base_id: Option<String>,
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
    pub end_column: u32,
    /// The location's (or thread-flow step's) message.
    pub message: Option<String>,
    /// A code-flow step's role when the tool says (CodeQL:
    /// `source`/`step`/`sink`).
    pub role: Option<String>,
}

impl SarifLocation {
    /// The location as a path relative to `root` (the analysed source
    /// root): relative URIs as they are, `file://` URIs under `root`
    /// stripped of it; anything else (another tree, a URL) is `None`.
    pub fn relative_path(&self, root: &Path) -> Option<String> {
        let uri = self.uri.as_str();
        if let Some(rest) = uri.strip_prefix("file://") {
            // `file:///abs` (empty authority) or `file://localhost/abs`.
            let path = rest.strip_prefix("localhost").unwrap_or(rest);
            let path = Path::new(path);
            let relative = path.strip_prefix(root).ok().or_else(|| {
                let canonical = root.canonicalize().ok()?;
                path.strip_prefix(canonical).ok()
            })?;
            return Some(relative.to_string_lossy().replace('\\', "/"));
        }
        if uri.contains("://") || uri.starts_with('/') {
            return Path::new(uri)
                .strip_prefix(root)
                .ok()
                .map(|p| p.to_string_lossy().replace('\\', "/"));
        }
        let clean = uri.trim_start_matches("./");
        (!clean.is_empty() && !clean.split('/').any(|part| part == "..")).then(|| clean.to_string())
    }
}

/// One result.
#[derive(Debug, Clone, Default)]
pub struct SarifResult {
    pub rule_id: String,
    /// `level` of the result (else the rule's default), when given.
    pub level: Option<String>,
    /// The message with its `[text](id)` links and `{n}` placeholders
    /// rendered as text.
    pub message: String,
    /// The primary location (`locations[0]`).
    pub location: Option<SarifLocation>,
    /// Each thread flow of each code flow, in order: a path's hops from
    /// source to sink.
    pub flows: Vec<Vec<SarifLocation>>,
    pub related: Vec<SarifLocation>,
    /// A stable key the tool gave (`partialFingerprints`, first by name).
    pub fingerprint: Option<String>,
}

/// Parse a SARIF log's text.
pub fn parse(text: &str) -> Result<Vec<SarifRun>, String> {
    let log: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    parse_value(&log)
}

/// Parse a SARIF log already read as JSON.
pub fn parse_value(log: &Value) -> Result<Vec<SarifRun>, String> {
    let Some(runs) = log.get("runs").and_then(Value::as_array) else {
        return Err("not a SARIF log: no `runs` array".to_string());
    };
    if let Some(version) = log
        .get("version")
        .and_then(Value::as_str)
        .filter(|v| *v != super::VERSION)
    {
        return Err(format!(
            "SARIF version {version} is not supported (expected {})",
            super::VERSION
        ));
    }
    Ok(runs.iter().map(parse_run).collect())
}

fn text_of(value: &Value) -> Option<String> {
    value
        .get("text")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            value
                .get("markdown")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

fn rule_meta(descriptor: &Value) -> Option<RuleMeta> {
    let id = descriptor.get("id")?.as_str()?.to_string();
    let props = descriptor.get("properties").cloned().unwrap_or(Value::Null);
    let prop_str = |key: &str| props.get(key).and_then(Value::as_str).map(str::to_string);
    let security_severity = match props.get("security-severity") {
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    };
    Some(RuleMeta {
        name: descriptor
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| prop_str("name")),
        short_description: descriptor.get("shortDescription").and_then(text_of),
        full_description: descriptor.get("fullDescription").and_then(text_of),
        help_uri: descriptor
            .get("helpUri")
            .and_then(Value::as_str)
            .map(str::to_string),
        tags: props
            .get("tags")
            .and_then(Value::as_array)
            .map(|tags| {
                tags.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        precision: prop_str("precision"),
        problem_severity: prop_str("problem.severity"),
        security_severity,
        kind: prop_str("kind"),
        level: descriptor
            .pointer("/defaultConfiguration/level")
            .and_then(Value::as_str)
            .map(str::to_string),
        id,
    })
}

fn parse_run(run: &Value) -> SarifRun {
    let driver = run.pointer("/tool/driver").cloned().unwrap_or(Value::Null);
    let descriptors = |component: &Value| -> Vec<Value> {
        component
            .get("rules")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let driver_rules = descriptors(&driver);
    let extensions: Vec<Vec<Value>> = run
        .pointer("/tool/extensions")
        .and_then(Value::as_array)
        .map(|exts| exts.iter().map(descriptors).collect())
        .unwrap_or_default();
    let mut rules = BTreeMap::new();
    for descriptor in driver_rules.iter().chain(extensions.iter().flatten()) {
        if let Some(meta) = rule_meta(descriptor) {
            rules.entry(meta.id.clone()).or_insert(meta);
        }
    }
    let artifacts: Vec<String> = run
        .get("artifacts")
        .and_then(Value::as_array)
        .map(|artifacts| {
            artifacts
                .iter()
                .map(|a| {
                    a.pointer("/location/uri")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default();

    let mut results = Vec::new();
    for result in run
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        // The rule: `ruleId`, else `rule.id`, else the descriptor the
        // index names (in the driver, or in the extension
        // `rule.toolComponent.index` names).
        let component_rules = match result.pointer("/rule/toolComponent/index") {
            Some(index) => index
                .as_u64()
                .and_then(|i| extensions.get(i as usize))
                .unwrap_or(&driver_rules),
            None => &driver_rules,
        };
        let by_index = result
            .get("ruleIndex")
            .or_else(|| result.pointer("/rule/index"))
            .and_then(Value::as_u64)
            .and_then(|i| component_rules.get(i as usize));
        let rule_id = result
            .get("ruleId")
            .and_then(Value::as_str)
            .or_else(|| result.pointer("/rule/id").and_then(Value::as_str))
            .or_else(|| by_index.and_then(|d| d.get("id")).and_then(Value::as_str));
        let Some(rule_id) = rule_id.map(str::to_string) else {
            continue;
        };
        let descriptor = by_index.or_else(|| {
            component_rules
                .iter()
                .find(|d| d.get("id").and_then(Value::as_str) == Some(rule_id.as_str()))
        });
        let message = render_message(result.get("message"), descriptor);
        let location = result
            .get("locations")
            .and_then(Value::as_array)
            .and_then(|locations| locations.first())
            .and_then(|loc| read_location(loc, &artifacts));
        let flows = result
            .get("codeFlows")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .flat_map(|flow| {
                flow.get("threadFlows")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            })
            .map(|thread| {
                thread
                    .get("locations")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|step| {
                        let mut loc = read_location(step.get("location")?, &artifacts)?;
                        loc.role = step
                            .get("taxa")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .find_map(|taxon| {
                                taxon
                                    .pointer("/properties/CodeQL~1DataflowRole")
                                    .and_then(Value::as_str)
                            })
                            .or_else(|| {
                                step.get("kinds")
                                    .and_then(Value::as_array)
                                    .and_then(|kinds| kinds.first())
                                    .and_then(Value::as_str)
                            })
                            .map(str::to_string);
                        Some(loc)
                    })
                    .collect::<Vec<_>>()
            })
            .filter(|steps: &Vec<SarifLocation>| !steps.is_empty())
            .collect();
        let related = result
            .get("relatedLocations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|loc| read_location(loc, &artifacts))
            .collect();
        let fingerprint = result
            .get("fingerprints")
            .or_else(|| result.get("partialFingerprints"))
            .and_then(Value::as_object)
            .and_then(|prints| {
                let mut keys: Vec<&String> = prints.keys().collect();
                keys.sort();
                keys.first()
                    .and_then(|key| prints.get(*key))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            });
        let level = result
            .get("level")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| rules.get(&rule_id).and_then(|meta| meta.level.clone()));
        results.push(SarifResult {
            rule_id,
            level,
            message,
            location,
            flows,
            related,
            fingerprint,
        });
    }

    let errors = run
        .get("invocations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|inv| {
            [
                "toolExecutionNotifications",
                "toolConfigurationNotifications",
            ]
            .into_iter()
            .filter_map(|key| inv.get(key).and_then(Value::as_array))
            .flatten()
            .cloned()
            .collect::<Vec<_>>()
        })
        .filter(|n| n.get("level").and_then(Value::as_str) == Some("error"))
        .filter_map(|n| n.get("message").and_then(text_of))
        .collect();

    SarifRun {
        tool: driver
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        tool_version: driver
            .get("semanticVersion")
            .or_else(|| driver.get("version"))
            .and_then(Value::as_str)
            .map(str::to_string),
        rules,
        results,
        errors,
    }
}

fn read_location(loc: &Value, artifacts: &[String]) -> Option<SarifLocation> {
    let physical = loc.get("physicalLocation")?;
    let artifact = physical.get("artifactLocation")?;
    let uri = artifact
        .get("uri")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            artifact
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|i| artifacts.get(i as usize).cloned())
        })
        .filter(|uri| !uri.is_empty())?;
    let region = physical.get("region");
    let num = |key: &str| {
        region
            .and_then(|r| r.get(key))
            .and_then(Value::as_u64)
            .map_or(0, |n| n as u32)
    };
    let line = num("startLine");
    Some(SarifLocation {
        uri: percent_decode(&uri),
        uri_base_id: artifact
            .get("uriBaseId")
            .and_then(Value::as_str)
            .map(str::to_string),
        line,
        column: num("startColumn"),
        end_line: match num("endLine") {
            0 => line,
            n => n,
        },
        end_column: num("endColumn"),
        message: loc.get("message").and_then(text_of),
        role: None,
    })
}

/// A result's message: `text` (else the rule's `messageStrings[id]`) with
/// `{n}` replaced by `arguments[n]` and `[text](target)` links read as
/// `text`.
fn render_message(message: Option<&Value>, descriptor: Option<&Value>) -> String {
    let Some(message) = message else {
        return String::new();
    };
    let template = message
        .get("text")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            let id = message.get("id")?.as_str()?;
            descriptor?
                .pointer(&format!("/messageStrings/{id}/text"))?
                .as_str()
                .map(str::to_string)
        })
        .unwrap_or_default();
    let arguments: Vec<&str> = message
        .get("arguments")
        .and_then(Value::as_array)
        .map(|args| args.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut text = template;
    for (index, argument) in arguments.iter().enumerate() {
        text = text.replace(&format!("{{{index}}}"), argument);
    }
    strip_links(&text)
}

/// `[text](target)` → `text`; `\[` / `\]` escapes → brackets.
fn strip_links(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' if matches!(chars.get(i + 1), Some('[' | ']')) => {
                out.push(chars[i + 1]);
                i += 2;
            }
            '[' => {
                // Find the closing `](…)`.
                let close = (i + 1..chars.len()).find(|&j| chars[j] == ']' && chars[j - 1] != '\\');
                match close {
                    Some(j) if chars.get(j + 1) == Some(&'(') => {
                        match (j + 2..chars.len()).find(|&k| chars[k] == ')') {
                            Some(k) => {
                                out.extend(&chars[i + 1..j]);
                                i = k + 1;
                            }
                            None => {
                                out.push('[');
                                i += 1;
                            }
                        }
                    }
                    _ => {
                        out.push('[');
                        i += 1;
                    }
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escaped = (bytes[i] == b'%' && i + 2 < bytes.len())
            .then(|| text.get(i + 1..i + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        if let Some(byte) = escaped {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_and_placeholders_render_as_text() {
        assert_eq!(
            strip_links("This path depends on a [user-provided value](1)."),
            "This path depends on a user-provided value."
        );
        assert_eq!(strip_links("a \\[b\\] [c](2) [d] e"), "a [b] c [d] e");
        let message = serde_json::json!({"text": "{0} flows to [{1}](1)", "arguments": ["x", "y"]});
        assert_eq!(render_message(Some(&message), None), "x flows to y");
        let by_id = serde_json::json!({"id": "default"});
        let descriptor = serde_json::json!({"messageStrings": {"default": {"text": "hi"}}});
        assert_eq!(render_message(Some(&by_id), Some(&descriptor)), "hi");
    }

    #[test]
    fn locations_resolve_against_the_source_root() {
        let loc = |uri: &str| SarifLocation {
            uri: uri.to_string(),
            ..SarifLocation::default()
        };
        let root = Path::new("/w/proj");
        assert_eq!(
            loc("src/A.java").relative_path(root).as_deref(),
            Some("src/A.java")
        );
        assert_eq!(
            loc("./src/A.java").relative_path(root).as_deref(),
            Some("src/A.java")
        );
        assert_eq!(
            loc("file:///w/proj/src/A.java")
                .relative_path(root)
                .as_deref(),
            Some("src/A.java")
        );
        assert_eq!(loc("file:///elsewhere/A.java").relative_path(root), None);
        assert_eq!(loc("../up.java").relative_path(root), None);
        assert_eq!(loc("https://x/y").relative_path(root), None);
        assert_eq!(percent_decode("a%20b%2Fc"), "a b/c");
    }

    #[test]
    fn a_log_that_is_not_sarif_is_an_error() {
        assert!(parse("[]").is_err());
        assert!(parse("{\"version\": \"2.0.0\", \"runs\": []}").is_err());
        assert!(parse("not json").is_err());
        assert_eq!(
            parse("{\"version\": \"2.1.0\", \"runs\": []}")
                .unwrap()
                .len(),
            0
        );
    }
}

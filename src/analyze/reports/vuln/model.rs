use super::Serialize;

// =============================================================================
// Vulnerability scan (inference-based engine + concurrency lint)
// =============================================================================

/// One rendered vulnerability finding, unified across the inference engine
/// (missing-guard / unsanitized-flow) and the concurrency lint.
#[derive(Debug, Clone, Serialize)]
pub struct VulnFindingOut {
    /// Template / rule id (e.g. `missing_dominator_check`, `lossy_send`).
    pub kind: String,
    /// The vulnerability *property* template this finding instantiates
    /// (`missing_dominator_check`, `reaches_without_sanitizer`, `must_follow`,
    /// `deviant_frequency`). Stable across the human rule id so concurrency
    /// must-follow-shaped findings (lossy-send-without-delivery) and the
    /// inference-engine findings share one taxonomy.
    pub template: String,
    /// Heuristic class label when one applies (e.g. `BAC`, `IDOR`, `SQL injection`).
    pub class: Option<String>,
    /// How the signature was inferred (`frequency`, `name`, `concurrency`).
    pub origin: String,
    pub file: String,
    pub line: u32,
    /// The function the finding sits in (or the source symbol for taint).
    pub symbol: String,
    pub confidence: f64,
    /// SARIF/severity bucket derived from confidence (`error`/`warning`/`note`).
    pub severity: String,
    pub message: String,
}

/// Map a confidence score to a SARIF `level` / severity bucket. High-confidence
/// findings (inferred norms shared by most call sites, or the fixed-precision
/// concurrency lint) escalate to `error`; mid-band to `warning`; the long tail
/// to `note`.
pub(crate) fn severity_for(confidence: f64) -> &'static str {
    if confidence >= 0.85 {
        "error"
    } else if confidence >= 0.6 {
        "warning"
    } else {
        "note"
    }
}

/// Result of a whole-project vulnerability scan.
#[derive(Debug, Clone, Serialize)]
pub struct VulnReport {
    pub findings: Vec<VulnFindingOut>,
    pub missing_guard_count: usize,
    pub taint_count: usize,
    pub concurrency_count: usize,
    pub scanned_functions: usize,
}

impl VulnReport {
    /// Human-readable rendering for non-JSON CLI output.
    pub fn render_human(&self) -> String {
        let mut out = format!(
            "Vulnerability scan — {} finding(s) across {} function(s)\n  \
             missing-guard: {}  unsanitized-flow: {}  concurrency: {}\n",
            self.findings.len(),
            self.scanned_functions,
            self.missing_guard_count,
            self.taint_count,
            self.concurrency_count,
        );
        const RENDER_CAP: usize = 60;
        for f in self.findings.iter().take(RENDER_CAP) {
            let class = f
                .class
                .as_ref()
                .map(|c| format!(" [{c}]"))
                .unwrap_or_default();
            out.push_str(&format!(
                "  - {}:{} ({}{}, {:.0}% via {}) {}\n    {}\n",
                f.file,
                f.line,
                f.kind,
                class,
                f.confidence * 100.0,
                f.origin,
                f.symbol,
                f.message,
            ));
        }
        if self.findings.len() > RENDER_CAP {
            out.push_str(&format!(
                "  ... and {} more (use --json for the full list, or raise --min-confidence)\n",
                self.findings.len() - RENDER_CAP,
            ));
        }
        out
    }

    /// Render the scan as a [SARIF 2.1.0] log — the interchange format GitHub
    /// Advanced Security, Defender, and most code-scanning dashboards ingest.
    ///
    /// One `reportingDescriptor` (rule) per distinct template kind seen, and one
    /// `result` per finding with `ruleId`, a confidence→`level` mapping, the
    /// message, and a physical `artifactLocation`/`region`. Confidence is also
    /// carried verbatim under `properties` so downstream tools can re-rank.
    ///
    /// [SARIF 2.1.0]: https://docs.oasis-open.org/sarif/sarif/v2.1.0/sarif-v2.1.0.html
    pub fn to_sarif(&self) -> serde_json::Value {
        use serde_json::json;

        // Distinct templates → SARIF rule descriptors (stable, sorted).
        let mut rule_ids: Vec<&str> = self.findings.iter().map(|f| f.template.as_str()).collect();
        rule_ids.sort_unstable();
        rule_ids.dedup();
        let rules: Vec<serde_json::Value> = rule_ids
            .iter()
            .map(|id| {
                json!({
                    "id": id,
                    "name": id,
                    "shortDescription": { "text": template_description(id) },
                })
            })
            .collect();

        let results: Vec<serde_json::Value> = self
            .findings
            .iter()
            .map(|f| {
                json!({
                    "ruleId": f.template,
                    "level": f.severity,
                    "message": { "text": f.message },
                    "locations": [{
                        "physicalLocation": {
                            "artifactLocation": { "uri": f.file },
                            "region": { "startLine": f.line.max(1) }
                        },
                        "logicalLocations": [{ "fullyQualifiedName": f.symbol }]
                    }],
                    "properties": {
                        "kind": f.kind,
                        "class": f.class,
                        "origin": f.origin,
                        "confidence": f.confidence,
                    }
                })
            })
            .collect();

        json!({
            "$schema": "https://docs.oasis-open.org/sarif/sarif/v2.1.0/sarif-v2.1.0.json",
            "version": "2.1.0",
            "runs": [{
                "tool": {
                    "driver": {
                        "name": "codegraph",
                        "informationUri": "https://github.com/coleleavitt/codegraph-rs",
                        "version": env!("CARGO_PKG_VERSION"),
                        "rules": rules,
                    }
                },
                "results": results,
            }]
        })
    }

    /// Render the scan as a standalone HTML report (no external assets) — the
    /// "hand it to management" artifact. Summary counts up top, then a
    /// severity-ordered table with file:line, template, class, confidence, and
    /// message. All dynamic text is HTML-escaped.
    pub fn to_html(&self) -> String {
        let mut rows = String::new();
        for f in &self.findings {
            let class = f.class.as_deref().unwrap_or("");
            rows.push_str(&format!(
                "<tr class=\"sev-{sev}\">\
                 <td class=\"sev\">{sev}</td>\
                 <td class=\"loc\">{file}:{line}</td>\
                 <td>{template}</td>\
                 <td>{class}</td>\
                 <td class=\"sym\">{symbol}</td>\
                 <td class=\"conf\">{conf:.0}%</td>\
                 <td>{msg}</td></tr>\n",
                sev = html_escape(&f.severity),
                file = html_escape(&f.file),
                line = f.line,
                template = html_escape(&f.template),
                class = html_escape(class),
                symbol = html_escape(&f.symbol),
                conf = f.confidence * 100.0,
                msg = html_escape(&f.message),
            ));
        }
        if self.findings.is_empty() {
            rows.push_str(
                "<tr><td colspan=\"7\" class=\"empty\">No findings at or above the \
                 confidence threshold.</td></tr>\n",
            );
        }

        format!(
            "<!DOCTYPE html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<title>codegraph vulnerability report</title>\n<style>\
:root{{color-scheme:light dark}}\
body{{font:14px/1.5 system-ui,sans-serif;margin:2rem;max-width:1100px}}\
h1{{font-size:1.4rem;margin:0 0 .25rem}}\
.sub{{color:#888;margin:0 0 1.5rem}}\
.cards{{display:flex;gap:1rem;flex-wrap:wrap;margin-bottom:1.5rem}}\
.card{{border:1px solid #8884;border-radius:8px;padding:.75rem 1rem;min-width:7rem}}\
.card .n{{font-size:1.6rem;font-weight:700}}\
.card .l{{color:#888;font-size:.8rem;text-transform:uppercase;letter-spacing:.04em}}\
table{{border-collapse:collapse;width:100%;font-size:13px}}\
th,td{{text-align:left;padding:.4rem .6rem;border-bottom:1px solid #8883;vertical-align:top}}\
th{{position:sticky;top:0;background:#8881;font-weight:600}}\
.loc,.sym{{font-family:ui-monospace,monospace;white-space:nowrap}}\
.conf{{text-align:right;font-variant-numeric:tabular-nums}}\
td.sev{{text-transform:uppercase;font-size:.7rem;font-weight:700}}\
.sev-error td.sev{{color:#d33}}.sev-warning td.sev{{color:#e90}}.sev-note td.sev{{color:#39c}}\
.empty{{color:#888;text-align:center;padding:1.5rem}}\
</style></head><body>\n\
<h1>codegraph vulnerability report</h1>\n\
<p class=\"sub\">{total} finding(s) across {fns} scanned function(s) · codegraph {ver}</p>\n\
<div class=\"cards\">\
<div class=\"card\"><div class=\"n\">{total}</div><div class=\"l\">Findings</div></div>\
<div class=\"card\"><div class=\"n\">{mg}</div><div class=\"l\">Missing-guard</div></div>\
<div class=\"card\"><div class=\"n\">{taint}</div><div class=\"l\">Unsanitized-flow</div></div>\
<div class=\"card\"><div class=\"n\">{conc}</div><div class=\"l\">Concurrency</div></div>\
</div>\n\
<table><thead><tr><th>Sev</th><th>Location</th><th>Template</th><th>Class</th>\
<th>Symbol</th><th>Conf</th><th>Message</th></tr></thead>\n<tbody>\n{rows}</tbody></table>\n\
</body></html>\n",
            total = self.findings.len(),
            fns = self.scanned_functions,
            ver = env!("CARGO_PKG_VERSION"),
            mg = self.missing_guard_count,
            taint = self.taint_count,
            conc = self.concurrency_count,
            rows = rows,
        )
    }
}

/// Human-readable one-liner for a SARIF rule descriptor, keyed by template id.
fn template_description(template_id: &str) -> &'static str {
    match template_id {
        "missing_dominator_check" => {
            "A sink reached on a path that skips a guard most sibling call sites pass \
             (missing authorization / validation; BAC, IDOR)."
        }
        "reaches_without_sanitizer" => {
            "A tainted source reaches a sink with no sanitizer on the path \
             (IDOR, SSRF, injection)."
        }
        "must_follow" => {
            "An operation occurs but a required follow-up is absent on some path \
             (lossy-send-without-delivery, lock-without-unlock)."
        }
        "deviant_frequency" => "Statistical deviation from a learned norm.",
        _ => "Inferred vulnerability finding.",
    }
}

/// Minimal HTML text escaper for report cell contents.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

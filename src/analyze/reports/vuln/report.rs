use codegraph_analysis::concurrency;
use codegraph_analysis::vuln::TemplateKind;
use codegraph_analysis::vuln::mining::{MineConfig, mine_missing_guards};
use codegraph_analysis::vuln::taint_seed::scan_unsanitized_flows;

use super::*;

/// Whether a finding sits in test code (excluded from a security scan).
fn is_test_site(file: &str, symbol: &str) -> bool {
    let f = file.replace('\\', "/");
    f.starts_with("tests/")
        || f.contains("/tests/")
        || f.ends_with("_test.rs")
        || f.ends_with("_tests.rs")
        || f.contains("/test_")
        || symbol.starts_with("test_")
        || symbol.starts_with("test ")
}

/// Resolve an analysis node to `(file, line, name)` for rendering.
fn render_site(graph: &AnalysisGraph, id: &ANodeId) -> (String, u32, String) {
    match graph.get_node(id) {
        Some(n) => (
            n.file_path.to_string_lossy().into_owned(),
            n.span.start_line,
            n.name.clone(),
        ),
        None => (String::new(), 0, format!("node#{}", id.0)),
    }
}

/// Run the full inference engine + concurrency lint over the bridged graph.
///
/// * missing-guard mining and unsanitized-flow taint run over the call/dataflow
///   graph (no source needed);
/// * the concurrency lint re-parses each distinct on-disk source file (only
///   Rust is wired in the walker today).
///
/// Findings below `min_confidence` are dropped (concurrency findings are
/// high-precision and assigned a fixed 0.9). Sorted by confidence descending.
pub fn vuln_report(
    graph: &AnalysisGraph,
    workspace_root: &Path,
    min_confidence: f64,
) -> VulnReport {
    let mut findings: Vec<VulnFindingOut> = Vec::new();

    // 1. Missing-guard deviation (BAC / IDOR), inferred from corpus consistency.
    let guard_findings = mine_missing_guards(graph, &MineConfig::default());
    let missing_guard_count = guard_findings.len();
    for f in guard_findings {
        let (file, line, symbol) = render_site(graph, &f.site);
        findings.push(VulnFindingOut {
            kind: f.template.id().to_owned(),
            template: f.template.id().to_owned(),
            class: f.class,
            origin: f.origin.id().to_owned(),
            file,
            line,
            symbol,
            confidence: f.confidence,
            severity: severity_for(f.confidence).to_owned(),
            message: f.message,
        });
    }

    // 2. Unsanitized taint flows (IDOR / SSRF / injection), inferred seeds.
    let taint_findings = scan_unsanitized_flows(graph, &[]);
    let taint_count = taint_findings.len();
    for f in taint_findings {
        let (file, line, symbol) = render_site(graph, &f.site);
        findings.push(VulnFindingOut {
            kind: f.template.id().to_owned(),
            template: f.template.id().to_owned(),
            class: f.class,
            origin: f.origin.id().to_owned(),
            file,
            line,
            symbol,
            confidence: f.confidence,
            severity: severity_for(f.confidence).to_owned(),
            message: f.message,
        });
    }

    // 3. Concurrency / control-plane lint over distinct on-disk source files.
    let mut seen_files: HashSet<PathBuf> = HashSet::new();
    for func in graph.nodes_by_kind(ANodeKind::Function) {
        seen_files.insert(func.file_path.clone());
    }
    let mut concurrency_count = 0usize;
    let mut files: Vec<PathBuf> = seen_files.into_iter().collect();
    files.sort();
    for rel in files {
        let lang = match rel.extension().and_then(|e| e.to_str()) {
            Some("rs") => "rust",
            _ => continue,
        };
        let Ok(source) = std::fs::read_to_string(workspace_root.join(&rel)) else {
            continue;
        };
        for c in concurrency::analyze_source(lang, &source) {
            concurrency_count += 1;
            // A concurrency lint hit (lossy-send-without-delivery,
            // remove-then-best-effort-resend) is structurally an operation-A
            // that requires a follow-B which is absent on some path — i.e. the
            // `MustFollow` property template. Tagging it here is what wires that
            // otherwise-unemitted template into the taxonomy.
            findings.push(VulnFindingOut {
                kind: c.rule.id().to_owned(),
                template: TemplateKind::MustFollow.id().to_owned(),
                class: None,
                origin: "concurrency".to_owned(),
                file: rel.to_string_lossy().into_owned(),
                line: c.line as u32,
                symbol: c.function.unwrap_or_else(|| "<module>".to_owned()),
                confidence: 0.9,
                severity: severity_for(0.9).to_owned(),
                message: c.message,
            });
        }
    }

    let scanned_functions = graph.nodes_by_kind(ANodeKind::Function).len();
    // Test code dominates false positives (fixtures named `test_*`, `tests/`
    // helpers reaching `open`/`remove`); drop it from a security scan.
    findings.retain(|f| !is_test_site(&f.file, &f.symbol));
    // A missing-guard deviation is only a *security* finding when the inferred
    // guard reads like a security control (auth/validation → a class label).
    // Deviations around benign helpers (`new`, `default`, `builder`) are real
    // structural variation, not vulnerabilities — keep them out of a vuln scan.
    // Discovery stays rule-free; the label only decides security relevance.
    findings.retain(|f| f.kind != "missing_dominator_check" || f.class.is_some());
    findings.retain(|f| f.confidence >= min_confidence);
    findings.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line.cmp(&b.line))
    });

    VulnReport {
        findings,
        missing_guard_count,
        taint_count,
        concurrency_count,
        scanned_functions,
    }
}

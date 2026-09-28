//! CodeQL's real SARIF (`samples/owasp-benchmark-java.sarif`: three results
//! of CodeQL 2.27.1's `java-security-and-quality` on OWASP Benchmark v1.2 —
//! a `path-problem` with its code flow, a quality result whose related
//! location is in the JDK, and one in a file the fixture does not index)
//! read and mapped onto a project.

use std::path::Path;

use super::findings::{base_confidence, map_runs};
use crate::analyze::bugs::reach::tests::{call, span};
use crate::analyze::bugs::{Detector, Project};
use crate::analyze::sarif::read;

const SAMPLE: &str = include_str!("samples/owasp-benchmark-java.sarif");
const TEST_FILE: &str = "src/main/java/org/owasp/benchmark/testcode/BenchmarkTest00001.java";

fn runs() -> Vec<read::SarifRun> {
    read::parse(SAMPLE).expect("the sample is SARIF")
}

/// `BenchmarkTest00001`: `doGet` (lines 40-46) calls `doPost` (48-105),
/// which takes the servlet request, and `helper` (107-110) nothing calls.
fn project() -> Project {
    let request = "void (HttpServletRequest request, HttpServletResponse response)";
    Project::from_parts(
        Path::new("/bench"),
        vec![TEST_FILE.to_string()],
        vec![
            span("doGet", TEST_FILE, 40, 46, request),
            span("doPost", TEST_FILE, 48, 105, request),
            span("helper", TEST_FILE, 107, 110, "void ()"),
        ],
        vec![call("doGet", "doPost", TEST_FILE, 45)],
    )
    .with_entries(vec![], &[])
}

#[test]
fn the_sample_reads_rules_results_flows_and_related_places() {
    let runs = runs();
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    assert_eq!(run.tool, "CodeQL");
    assert_eq!(run.tool_version.as_deref(), Some("2.27.1"));
    assert_eq!(run.results.len(), 3);

    let path = &run.rules["java/path-injection"];
    assert_eq!(path.precision.as_deref(), Some("high"));
    assert_eq!(path.security_severity, Some(7.5));
    assert_eq!(path.kind.as_deref(), Some("path-problem"));
    assert_eq!(path.cwes(), ["CWE-22", "CWE-23", "CWE-36", "CWE-73"]);
    assert!(path.is_security());
    let quality = &run.rules["java/unused-parameter"];
    assert_eq!(quality.problem_severity.as_deref(), Some("recommendation"));
    assert!(!quality.is_security());

    let taint = &run.results[0];
    assert_eq!(taint.rule_id, "java/path-injection");
    assert_eq!(taint.message, "This path depends on a user-provided value.");
    let location = taint.location.as_ref().unwrap();
    assert_eq!((location.line, location.column), (72, 47));
    assert_eq!(location.uri_base_id.as_deref(), Some("%SRCROOT%"));
    assert_eq!(taint.flows.len(), 1);
    let hops: Vec<(u32, Option<&str>, Option<&str>)> = taint.flows[0]
        .iter()
        .map(|step| (step.line, step.role.as_deref(), step.message.as_deref()))
        .collect();
    assert_eq!(
        hops,
        [
            (61, Some("source"), Some("getValue(...) : String")),
            (61, Some("step"), Some("decode(...) : String")),
            (72, None, Some("fileName : String")),
            (72, Some("step"), Some("new File(...)")),
        ]
    );
    assert!(taint.fingerprint.is_some());

    let deprecated = &run.results[1];
    assert_eq!(
        deprecated.message,
        "Invoking URL.URL should be avoided because it has been deprecated."
    );
    assert_eq!(
        deprecated.related[0].uri,
        "file:///modules/java.base/java/net/URL.class"
    );
}

#[test]
fn results_map_to_findings_in_their_functions_with_the_path_as_evidence() {
    let project = project();
    let mapped = map_runs(&project, &runs());
    assert_eq!(mapped.findings.len(), 2, "{:#?}", mapped.findings);
    assert_eq!(
        mapped.outside_index, 1,
        "DataBaseServer.java is not indexed"
    );
    assert!(mapped.rules.contains_key("codeql::java/path-injection"));

    let taint = &mapped.findings[0];
    assert_eq!(taint.detector, Detector::CodeQL);
    assert_eq!(taint.rule, "codeql::java/path-injection");
    assert_eq!(
        (taint.file.as_str(), taint.line, taint.col),
        (TEST_FILE, 72, 46)
    );
    assert_eq!(taint.function.as_deref(), Some("m::doPost"));
    assert!(
        taint
            .message
            .ends_with("— reachable from request handler `m::doPost` (takes HttpServletRequest)"),
        "{}",
        taint.message
    );
    let notes: Vec<&str> = taint.evidence.iter().map(|e| e.note.as_str()).collect();
    assert_eq!(
        notes,
        [
            "source 1/4: getValue(...) : String",
            "step 2/4: decode(...) : String",
            "step 3/4: fileName : String",
            "step 4/4: new File(...)",
            "entry point: request handler `m::doPost` (takes HttpServletRequest)",
        ],
        "the related source duplicates the first hop; the entry closes the list"
    );
    let base = base_confidence(
        mapped.rules.get("codeql::java/path-injection"),
        Some("error"),
    );
    assert_eq!(base, 0.76);
    assert_eq!(
        taint.confidence, 0.865,
        "a handler reaches it: halfway to 0.97"
    );

    let quality = &mapped.findings[1];
    assert_eq!(quality.rule, "codeql::java/deprecated-call");
    assert_eq!(quality.function.as_deref(), Some("m::doGet"));
    assert!(
        quality.evidence.is_empty(),
        "the JDK's URL.class is no place in the project: {:?}",
        quality.evidence
    );
    assert_eq!(
        quality.confidence,
        base_confidence(
            mapped.rules.get("codeql::java/deprecated-call"),
            Some("note")
        ),
        "a quality result is not raised by reachability"
    );
}

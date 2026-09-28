/// The built-in subprocess rule gated on server context: the index's axum
/// route reaches one `pdftotext` (reported, with the path from the route),
/// a CLI helper's `git` wait is dropped, and a CLI's own `pdftotext` (a
/// document converter) stays, ranked lower.
#[test]
fn analyze_rules_reached_from_gates_on_server_context() {
    let (_dir, root) = temp_project();
    // The route scan runs on Cargo projects.
    support::write(
        &root.join("Cargo.toml"),
        "[package]\nname = \"srv\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\naxum = \"0.7\"\n",
    );
    support::write(
        &root.join("src/lib.rs"),
        r#"use axum::routing::post;
use axum::Router;
use std::process::Command;

pub fn app() -> Router {
    Router::new().route("/reindex", post(reindex))
}

pub async fn reindex() -> String {
    extract_pdf_text("a.pdf")
}

fn extract_pdf_text(path: &str) -> String {
    let out = Command::new("pdftotext").arg(path).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

pub fn git_head() -> String {
    let out = Command::new("git").arg("rev-parse").output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

pub fn convert(path: &str) -> String {
    let out = Command::new("pdftotext").arg(path).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}
"#,
    );
    init_fixture_files_only(&root);

    let report = run_analyze_json(&root, &["rules", "--builtin", "--no-saved"]);
    let findings: Vec<&serde_json::Value> = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["rule"] == "rust-subprocess-without-deadline")
        .collect();
    let found: Vec<(&str, u64, f64)> = findings
        .iter()
        .map(|f| {
            (
                f["function"].as_str().unwrap(),
                f["line"].as_u64().unwrap(),
                f["confidence"].as_f64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        found,
        [("extract_pdf_text", 14, 0.6), ("convert", 24, 0.45)],
        "{report}"
    );
    let notes: Vec<(&str, u64, &str)> = findings[0]["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["file"].as_str().unwrap(),
                e["line"].as_u64().unwrap(),
                e["note"].as_str().unwrap(),
            )
        })
        .collect();
    assert!(
        notes.iter().any(|(_, line, note)| *line == 14
            && note.starts_with("reached from route `POST /reindex` (`reindex`), 1 call away")),
        "{notes:#?}"
    );
    assert!(
        notes.contains(&("src/lib.rs", 10, "`reindex` calls `extract_pdf_text`")),
        "{notes:#?}"
    );
    assert!(
        findings[1]["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["note"].as_str().unwrap().starts_with("ranked lower: no route/")),
        "{report}"
    );
}

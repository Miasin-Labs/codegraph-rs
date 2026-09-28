//! Markdown documents in the graph: RFC sections as nodes, citations and
//! feature gates as edges (from documents and from Rust code), doc examples
//! kept out of code resolution, and the CLI showing a section.

use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use codegraph::types::{EdgeKind, Language, Node, NodeKind};
use codegraph::{CodeGraph, IndexOptions};

const TEST_HOME: &str = concat!(env!("CARGO_TARGET_TMPDIR"), "/codegraph-home");

const DROPCK: &str = "- Feature Name: `dropck_parametricity`
- RFC PR: [rust-lang/rfcs#1238](https://github.com/rust-lang/rfcs/pull/1238)

# Summary

Revise the Drop Check (`dropck`) part of Rust's static analyses.

# Detailed design

## Changes to the Drop-Check Rule

The drop-check rule no longer assumes parametricity.

```rust
fn helper() -> u32 { 1 }
```
";

const EYEPATCH: &str = "- Feature Name: `dropck_eyepatch`
- RFC PR: [rust-lang/rfcs#1327](https://github.com/rust-lang/rfcs/pull/1327)

# Summary

Refine the escape hatch from RFC 1238 so it names its type parameters.

# Motivation

See [the rule](1238-nonparametric-dropck.md#changes-to-the-drop-check-rule).
";

const LIB: &str = "#![feature(dropck_eyepatch)]

pub fn run() -> u32 {
    helper()
}
";

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn node(cg: &CodeGraph, name: &str, kind: NodeKind) -> Node {
    cg.get_nodes_by_name(name)
        .unwrap()
        .into_iter()
        .find(|n| n.kind == kind)
        .unwrap_or_else(|| panic!("no {kind} {name}"))
}

/// Who references `target`: `(source name, source kind)`.
fn referrers(cg: &CodeGraph, target: &Node) -> Vec<(String, NodeKind)> {
    let mut found: Vec<(String, NodeKind)> = cg
        .get_incoming_edges(&target.id)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == EdgeKind::References)
        .map(|e| {
            let source = cg.get_node(&e.source).unwrap().unwrap();
            (source.name, source.kind)
        })
        .collect();
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

async fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let temp = tempfile::Builder::new()
        .prefix("codegraph-markdown-")
        .tempdir()
        .unwrap();
    let root = temp.path().join("rfcs");
    write(&root, "text/1238-nonparametric-dropck.md", DROPCK);
    write(&root, "text/1327-dropck-param-eyepatch.md", EYEPATCH);
    write(&root, "src/lib.rs", LIB);
    let cg = CodeGraph::init_sync(&root).unwrap();
    assert!(
        cg.index_all(&IndexOptions::default())
            .await
            .unwrap()
            .success
    );
    cg.close();
    (temp, root)
}

#[tokio::test(flavor = "current_thread")]
async fn documents_link_rfcs_sections_and_features() {
    let (_temp, root) = fixture().await;
    let cg = CodeGraph::open_sync(&root).unwrap();

    // RFC 1327's Summary cites RFC 1238 by number.
    let rfc1238 = node(&cg, "RFC 1238", NodeKind::Module);
    assert_eq!(
        referrers(&cg, &rfc1238),
        vec![("Summary".to_string(), NodeKind::Section)]
    );
    // Its link with an anchor lands on the section, not the file.
    let rule = node(&cg, "Changes to the Drop-Check Rule", NodeKind::Section);
    assert_eq!((rule.start_line, rule.end_line), (10, 16));
    assert_eq!(
        referrers(&cg, &rule),
        vec![("Motivation".to_string(), NodeKind::Section)]
    );
    // `#![feature(dropck_eyepatch)]` in code reaches the RFC declaring it.
    let feature = node(&cg, "dropck_eyepatch", NodeKind::Constant);
    assert_eq!(feature.language, Language::Markdown);
    assert_eq!(
        referrers(&cg, &feature),
        vec![("lib.rs".to_string(), NodeKind::File)]
    );

    // The doc example `helper` is a node of the document, but code's
    // `helper()` never resolves to it.
    let helper = node(&cg, "helper", NodeKind::Function);
    assert_eq!(helper.language, Language::Markdown);
    assert!(
        cg.get_incoming_edges(&helper.id)
            .unwrap()
            .iter()
            .all(|e| e.kind == EdgeKind::Contains)
    );
    let run = node(&cg, "run", NodeKind::Function);
    assert!(cg.get_callees(&run.id, None).unwrap().is_empty());
    cg.close();
}

#[tokio::test(flavor = "current_thread")]
async fn editing_a_cited_document_relinks_it_on_sync() {
    let (_temp, root) = fixture().await;
    // Move the section down two lines: its node is replaced, and the link
    // into it must follow.
    write(
        &root,
        "text/1238-nonparametric-dropck.md",
        &DROPCK.replace("# Detailed design\n", "# Detailed design\n\nMore.\n"),
    );
    let cg = CodeGraph::open_sync(&root).unwrap();
    cg.sync(&IndexOptions::default()).await.unwrap();
    let rule = node(&cg, "Changes to the Drop-Check Rule", NodeKind::Section);
    assert_eq!(rule.start_line, 12);
    assert_eq!(
        referrers(&cg, &rule),
        vec![("Motivation".to_string(), NodeKind::Section)]
    );
    let rfc1238 = node(&cg, "RFC 1238", NodeKind::Module);
    assert_eq!(referrers(&cg, &rfc1238).len(), 1);
    cg.close();
}

fn run_cli(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .args(args)
        .current_dir(root)
        .env("CODEGRAPH_HOME", TEST_HOME)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_BACKGROUND_SYNC", "1")
        .stdin(Stdio::null())
        .output()
        .expect("spawn codegraph")
}

/// Stdout without terminal styling.
fn plain(output: &Output) -> String {
    let text = String::from_utf8_lossy(&output.stdout);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            // CSI: ESC [ … final byte in @..~
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) && next != '[' {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

#[tokio::test(flavor = "current_thread")]
async fn cli_finds_and_shows_an_rfc_section() {
    let (_temp, root) = fixture().await;
    let search = run_cli(&root, &["query", "drop check"]);
    assert!(search.status.success());
    let text = plain(&search);
    let first = text
        .lines()
        .find(|line| line.starts_with("section") || line.starts_with("function"))
        .unwrap_or_default();
    assert!(
        first.contains("Changes to the Drop-Check Rule"),
        "first hit should be the section:\n{text}"
    );

    let shown = run_cli(&root, &["node", "Changes to the Drop-Check Rule"]);
    assert!(shown.status.success());
    let text = plain(&shown);
    assert!(
        text.contains("text/1238-nonparametric-dropck.md:10"),
        "{text}"
    );
    assert!(
        text.contains("The drop-check rule no longer assumes parametricity."),
        "{text}"
    );
}

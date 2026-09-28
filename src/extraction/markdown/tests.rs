use super::*;

/// A small RFC in the rust-lang/rfcs shape: header list, nested sections,
/// tagged/untagged/other code blocks, citations and feature mentions.
pub(crate) const RFC_FIXTURE: &str = r#"- Feature Name: `dropck_eyepatch`
- Start Date: 2015-10-19
- RFC PR: [rust-lang/rfcs#1327](https://github.com/rust-lang/rfcs/pull/1327)
- Rust Issue: [rust-lang/rust#34761](https://github.com/rust-lang/rust/issues/34761)

# Summary
[summary]: #summary

Refine the unguarded-escape-hatch from RFC 1238 (nonparametric dropck)
so that it names the type parameters it is unsafe about.

# Motivation

See [the dropck RFC](1238-nonparametric-dropck.md#changes-to-the-drop-check-rule)
and rust-lang/rfcs#769. Code enables it with `#![feature(dropck_eyepatch)]`.

## Drop check

```rust
struct Wrapper<T>(Vec<T>);

unsafe impl<#[may_dangle] T> Drop for Wrapper<T> {
    fn drop(&mut self) {}
}
```

```
fn helper() -> u32 { 1 }
```

```
expr := expr '+' term
```

```text
fn not_parsed() {}
```

# Unresolved questions

The feature gate `generic_param_attrs` interacts with RFC #1327 itself.
"#;

fn extract(path: &str, source: &str) -> ExtractionResult {
    MarkdownExtractor::new(path, source).extract()
}

fn count(result: &ExtractionResult, kind: NodeKind) -> usize {
    result.nodes.iter().filter(|n| n.kind == kind).count()
}

#[test]
fn rfc_fixture_yield_is_pinned() {
    let result = extract("text/1327-dropck-param-eyepatch.md", RFC_FIXTURE);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    // File, RFC 1327, 4 sections, 1 declared feature, and the doc examples:
    // struct Wrapper (and its tuple field), its impl's `drop` method, and
    // the untagged `helper`.
    assert_eq!(count(&result, NodeKind::File), 1);
    assert_eq!(count(&result, NodeKind::Module), 1);
    assert_eq!(count(&result, NodeKind::Section), 4);
    assert_eq!(count(&result, NodeKind::Constant), 1);
    let examples: Vec<(&str, NodeKind)> = result
        .nodes
        .iter()
        .filter(|n| {
            !matches!(
                n.kind,
                NodeKind::File | NodeKind::Module | NodeKind::Section | NodeKind::Constant
            )
        })
        .map(|n| (n.name.as_str(), n.kind))
        .collect();
    assert_eq!(
        examples,
        vec![
            ("Wrapper", NodeKind::Struct),
            ("0", NodeKind::Field),
            ("drop", NodeKind::Method),
            ("helper", NodeKind::Function)
        ]
    );
    assert_eq!(result.nodes.len(), 11);
    assert_eq!(result.edges.len(), 10);
    let mut refs: Vec<(&str, u32)> = result
        .unresolved_references
        .iter()
        .map(|r| (r.reference_name.as_str(), r.line))
        .collect();
    refs.sort();
    assert_eq!(
        refs,
        vec![
            (
                "doc:text/1238-nonparametric-dropck%2Emd#changes-to-the-drop-check-rule",
                14
            ),
            ("feature:dropck_eyepatch", 15),
            ("feature:generic_param_attrs", 41),
            ("rfc:1238", 9),
            ("rfc:769", 15),
        ]
    );
}

#[test]
fn sections_nest_by_level_with_spans() {
    let result = extract("text/1327-dropck-param-eyepatch.md", RFC_FIXTURE);
    let section = |name: &str| {
        result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Section && n.name == name)
            .unwrap_or_else(|| panic!("no section {name}"))
    };
    let motivation = section("Motivation");
    let drop_check = section("Drop check");
    assert_eq!((motivation.start_line, motivation.end_line), (12, 37));
    assert_eq!((drop_check.start_line, drop_check.end_line), (17, 37));
    assert_eq!(
        drop_check.qualified_name,
        "text/1327-dropck-param-eyepatch.md::RFC 1327::Motivation::Drop check"
    );
    assert_eq!(
        section("Summary").docstring.as_deref(),
        Some(
            "Refine the unguarded-escape-hatch from RFC 1238 (nonparametric dropck) so that it names the type parameters it is unsafe about."
        )
    );
    let contains = |from: &Node, to: &Node| {
        result
            .edges
            .iter()
            .any(|e| e.kind == EdgeKind::Contains && e.source == from.id && e.target == to.id)
    };
    assert!(contains(motivation, drop_check));
    let rfc = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Module)
        .unwrap();
    assert_eq!(rfc.name, "RFC 1327");
    assert!(contains(rfc, motivation));
    assert_eq!(rfc.docstring, section("Summary").docstring);
    let feature = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Constant)
        .unwrap();
    assert_eq!(feature.name, "dropck_eyepatch");
    assert!(
        feature
            .qualified_name
            .ends_with("::feature(dropck_eyepatch)")
    );
    assert!(contains(rfc, feature));
    // The byte span covers the heading through the section's last line.
    let span = drop_check.byte_range().unwrap();
    assert!(RFC_FIXTURE[span.clone()].starts_with("## Drop check"));
    assert!(RFC_FIXTURE[span].ends_with("fn not_parsed() {}\n```"));
}

#[test]
fn doc_examples_are_markdown_nodes_inside_their_section() {
    let result = extract("text/1327-dropck-param-eyepatch.md", RFC_FIXTURE);
    let wrapper = result.nodes.iter().find(|n| n.name == "Wrapper").unwrap();
    let helper = result.nodes.iter().find(|n| n.name == "helper").unwrap();
    for example in [wrapper, helper] {
        assert_eq!(example.language, Language::Markdown);
        assert!(
            example.qualified_name.starts_with(
                "text/1327-dropck-param-eyepatch.md::RFC 1327::Motivation::Drop check::"
            ),
            "{}",
            example.qualified_name
        );
    }
    // Lines and bytes are the document's, not the block's.
    assert_eq!(wrapper.start_line, 20);
    let bytes = wrapper.byte_range().unwrap();
    assert_eq!(&RFC_FIXTURE[bytes], "struct Wrapper<T>(Vec<T>);");
    let drop_check = result
        .nodes
        .iter()
        .find(|n| n.name == "Drop check")
        .unwrap();
    assert!(result.edges.iter().any(|e| e.kind == EdgeKind::Contains
        && e.source == drop_check.id
        && e.target == wrapper.id));
    // A block's own references never leave it.
    assert!(
        result
            .unresolved_references
            .iter()
            .all(|r| r.language == Some(Language::Markdown))
    );
}

#[test]
fn plain_documents_have_no_rfc_node_and_list_indented_fences_parse() {
    let source = "Intro\n=====\n\n1. Step\n\n    ```rust\n    fn nested() {}\n    ```\n";
    let result = extract("docs/guide.md", source);
    assert_eq!(count(&result, NodeKind::Module), 0);
    assert_eq!(count(&result, NodeKind::Section), 1);
    let nested = result.nodes.iter().find(|n| n.name == "nested").unwrap();
    assert_eq!(nested.start_line, 7);
    assert_eq!(nested.start_column, 4);
    assert_eq!(nested.start_byte, None);
    let section = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Section)
        .unwrap();
    assert_eq!(section.qualified_name, "docs/guide.md::Intro");
}

#[test]
fn budgets_bound_sections_blocks_and_references() {
    let mut source = String::new();
    for i in 0..50 {
        source.push_str(&format!(
            "# H{i}\nRFC {}\n```rust\nfn f{i}() {{}}\n```\n",
            i + 1
        ));
    }
    let budget = Budget {
        max_sections: 10,
        max_code_blocks: 3,
        max_block_bytes: 1024,
        max_code_bytes: 1024,
        max_example_nodes: 100,
        max_references: 5,
    };
    let result = MarkdownExtractor::new("a.md", &source)
        .with_budget(budget)
        .extract();
    assert_eq!(count(&result, NodeKind::Section), 10);
    assert_eq!(count(&result, NodeKind::Function), 3);
    assert_eq!(result.unresolved_references.len(), 5);
    assert_eq!(result.errors.len(), 3, "{:?}", result.errors);
    assert!(
        result
            .errors
            .iter()
            .all(|e| e.severity == Severity::Warning)
    );
}

#[test]
fn duplicate_headings_get_distinct_ids() {
    let source = "# A\n## Examples\n# B\n## Examples\n";
    let result = extract("x.md", source);
    let ids: HashSet<&str> = result.nodes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(ids.len(), result.nodes.len());
}

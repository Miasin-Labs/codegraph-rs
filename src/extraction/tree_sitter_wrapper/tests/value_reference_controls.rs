use std::ffi::OsStr;

use super::super::TreeSitterExtractor;
use super::value_references::{value_reference_edges, value_reference_readers};
use crate::extraction::languages::extractor_for;
use crate::types::{EdgeKind, ExtractionResult, Language, NodeKind};

fn extract(path: &str, language: Language, source: &str) -> ExtractionResult {
    TreeSitterExtractor::new(path, source, Some(language), extractor_for(language)).extract()
}

#[test]
fn local_rebindings_prune_shared_value_targets_in_every_shadowing_language() {
    // Given: each source negative control with a shared value and a local rebinding.
    let cases = [
        (
            "rust",
            "shadow.rs",
            Language::Rust,
            "const TIMEOUT: u32 = 30;\nfn uses_const() -> u32 { TIMEOUT }\nfn shadows() -> u32 { let TIMEOUT = 5; TIMEOUT }",
        ),
        (
            "go",
            "shadow.go",
            Language::Go,
            "package main\nconst Timeout = 30\nfunc usesConst() int { return Timeout }\nfunc shadows() int { Timeout := 5; return Timeout }",
        ),
        (
            "c",
            "shadow.c",
            Language::C,
            "static const int TIMEOUT = 30;\nint uses_const(void) { return TIMEOUT; }\nint shadows(void) { int TIMEOUT = 5; return TIMEOUT; }",
        ),
        (
            "java",
            "Shadow.java",
            Language::Java,
            "class Shadow { static final int TIMEOUT = 30; int usesConst() { return TIMEOUT; } int shadows() { int TIMEOUT = 5; return TIMEOUT; } }",
        ),
        (
            "csharp",
            "Shadow.cs",
            Language::Csharp,
            "class Shadow { const int TIMEOUT = 30; int UsesConst() { return TIMEOUT; } int Shadows() { int TIMEOUT = 5; return TIMEOUT; } }",
        ),
        (
            "scala",
            "Shadow.scala",
            Language::Scala,
            "object Config { val TIMEOUT = 30; def usesConst(): Int = TIMEOUT; def shadows(): Int = { val TIMEOUT = 5; TIMEOUT } }",
        ),
        (
            "kotlin",
            "Shadow.kt",
            Language::Kotlin,
            "object Config { const val TIMEOUT = 30; fun usesConst(): Int = TIMEOUT; fun shadows(): Int { val TIMEOUT = 5; return TIMEOUT } }",
        ),
        (
            "swift",
            "Shadow.swift",
            Language::Swift,
            "enum Config { static let TIMEOUT = 30; static func usesConst() -> Int { return TIMEOUT }; static func shadows() -> Int { let TIMEOUT = 5; return TIMEOUT } }",
        ),
        (
            "dart",
            "shadow.dart",
            Language::Dart,
            "const TIMEOUT = 30; class C { int usesConst() => TIMEOUT; int shadows() { const TIMEOUT = 5; return TIMEOUT; } }",
        ),
        (
            "pascal",
            "shadow.pas",
            Language::Pascal,
            "unit Shadow;\ninterface\nconst TIMEOUT = 30;\nimplementation\nfunction UsesConst: Integer;\nbegin UsesConst := TIMEOUT; end;\nfunction Shadows: Integer;\nconst TIMEOUT = 5;\nbegin Shadows := TIMEOUT; end;\nend.",
        ),
    ];

    // When/Then: extraction drops the globally ambiguous target for every language.
    for (label, path, language, source) in cases {
        let result = extract(path, language, source);
        assert!(
            value_reference_readers(&result, "TIMEOUT").is_empty(),
            "{label}: {:?}",
            value_reference_edges(&result)
        );
    }
}

#[test]
fn per_instance_and_mutable_members_are_not_value_targets() {
    // Given: immutable-looking instance members and mutable static properties from source controls.
    let cases = [
        (
            "java",
            "A.java",
            Language::Java,
            "class A { final int instanceId = 1; int id() { return instanceId; } }",
            "instanceId",
        ),
        (
            "csharp",
            "A.cs",
            Language::Csharp,
            "class A { readonly int instanceId = 1; int Id() { return instanceId; } }",
            "instanceId",
        ),
        (
            "php",
            "A.php",
            Language::Php,
            "<?php class A { public static $counter = 0; function count() { return self::$counter; } }",
            "counter",
        ),
        (
            "scala",
            "A.scala",
            Language::Scala,
            "class A { val MaxItems = 100; def within(n: Int): Int = if (n < MaxItems) n else MaxItems }",
            "MaxItems",
        ),
        (
            "kotlin",
            "A.kt",
            Language::Kotlin,
            "class A { val instanceField = 1; fun value(): Int = instanceField }",
            "instanceField",
        ),
        (
            "swift",
            "A.swift",
            Language::Swift,
            "struct A { let instanceField = 1; func value() -> Int { instanceField } }",
            "instanceField",
        ),
        (
            "dart",
            "a.dart",
            Language::Dart,
            "class A { final int instanceField = 1; int value() => instanceField; }",
            "instanceField",
        ),
    ];

    // When/Then: extraction emits no value-reference edge to per-instance state.
    for (label, path, language, source, target) in cases {
        let result = extract(path, language, source);
        assert!(
            value_reference_readers(&result, target).is_empty(),
            "{label} target {target}"
        );
    }
}

#[test]
fn macro_prefixed_c_prototypes_do_not_become_value_targets() {
    // Given: macro-prefixed prototypes beside a real file constant.
    let source = "typedef enum { CURLE_OK, CURLE_FAIL } CURLcode;\nCURL_EXTERN CURLcode curl_easy_init(int x);\nCURL_EXTERN CURLcode curl_easy_setopt(int y);\nstatic const int REAL_LIMIT = 42;\nint use_real(void) { return REAL_LIMIT; }";

    // When: the C file is extracted.
    let result = extract("api.c", Language::C, source);

    // Then: CURLcode is only a type while the real constant keeps its reader edge.
    assert!(!result.nodes.iter().any(|node| {
        node.name == "CURLcode" && matches!(node.kind, NodeKind::Constant | NodeKind::Variable)
    }));
    assert_eq!(value_reference_readers(&result, "REAL_LIMIT"), ["use_real"]);
}

#[test]
fn generated_files_and_disabled_extraction_emit_no_value_edges() {
    // Given: the same readable fixture under generated and explicitly-disabled extractors.
    let source = "export const TABLE_CONFIG = { rows: 10 }; export function rowCount() { return TABLE_CONFIG.rows; }";
    let generated = extract("config.generated.ts", Language::Typescript, source);
    let mut disabled = TreeSitterExtractor::new(
        "config.ts",
        source,
        Some(Language::Typescript),
        extractor_for(Language::Typescript),
    );
    disabled.value_references_enabled = false;

    // When: both files are extracted.
    let disabled = disabled.extract();

    // Then: neither gate emits a value edge.
    assert!(value_reference_edges(&generated).is_empty());
    assert!(value_reference_edges(&disabled).is_empty());
    assert!(!super::super::value_references::value_references_enabled(
        Some(OsStr::new("0"))
    ));
    assert!(super::super::value_references::value_references_enabled(
        None
    ));
}

#[test]
fn rust_language_hook_survives_without_duplicate_value_edges() {
    // Given: one generic constant read and one unit-struct value path handled by Rust's hook.
    let source = "const MAX_RETRIES: u32 = 3; struct ConstantFoldPass; fn build() { let retries = MAX_RETRIES; let pass = ConstantFoldPass; }";

    // When: the Rust file is extracted.
    let result = extract("lib.rs", Language::Rust, source);

    // Then: the generic edge is unique and the language-specific unresolved hook remains unique.
    assert_eq!(value_reference_edges(&result).len(), 1);
    let references: Vec<_> = result
        .unresolved_references
        .iter()
        .filter(|reference| reference.reference_kind == EdgeKind::References)
        .map(|reference| reference.reference_name.as_str())
        .collect();
    assert_eq!(references, ["ConstantFoldPass"]);
}

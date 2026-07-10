use super::ignore::{build_default_ignore, default_ignore_patterns, gitignore_ignores};
use super::parse::extract_from_source;
use super::store::hash_content;
use crate::resolution::frameworks::get_all_framework_resolvers;

#[test]
fn hash_content_is_sha256_hex() {
    // sha256("") and sha256("hello") — well-known vectors, identical to
    // Node's crypto.createHash('sha256').update(s).digest('hex').
    assert_eq!(
        hash_content(""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        hash_content("hello"),
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    );
}

#[test]
fn default_ignore_patterns_include_dirs_and_globs() {
    let patterns = default_ignore_patterns();
    assert!(patterns.contains(&"node_modules/".to_string()));
    assert!(patterns.contains(&"__pycache__/".to_string()));
    assert!(patterns.contains(&"*.egg-info/".to_string()));
    assert!(patterns.contains(&"cmake-build-*/".to_string()));
    assert!(patterns.contains(&"bazel-*/".to_string()));
    // first-party-prone names must NOT be listed
    assert!(!patterns.contains(&"src/".to_string()));
    assert!(!patterns.contains(&"lib/".to_string()));
}

#[test]
fn build_default_ignore_excludes_defaults_and_honors_negation() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".gitignore"), "!vendor/\nsecret.ts\n").unwrap();
    let ig = build_default_ignore(dir.path());

    // default dir ignored at any depth
    assert!(gitignore_ignores(&ig, "node_modules/pkg/index.js", false));
    assert!(gitignore_ignores(&ig, "a/b/node_modules/x.ts", false));
    // .gitignore negation re-includes a default
    assert!(!gitignore_ignores(&ig, "vendor/lib.go", false));
    // .gitignore additions apply
    assert!(gitignore_ignores(&ig, "secret.ts", false));
    // normal source kept
    assert!(!gitignore_ignores(&ig, "src/index.ts", false));
}

#[test]
fn codegraphignore_is_merged() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".codegraphignore"),
        "research/decompiled-references/\n",
    )
    .unwrap();
    let ig = build_default_ignore(dir.path());
    assert!(gitignore_ignores(
        &ig,
        "research/decompiled-references/all/generated.c",
        false
    ));
    assert!(!gitignore_ignores(&ig, "src/main.rs", false));
}

#[test]
fn extract_from_source_routes_to_file_level_only() {
    let result = extract_from_source("app.yaml", "name: test\n", None, None);
    assert!(result.nodes.is_empty());
    assert!(result.errors.is_empty());
    assert_eq!(result.duration_ms, 0.0);
}

#[test]
fn framework_registry_matches_ts_order_and_names() {
    // extract_from_source filters this registry by detected names — sanity
    // check the canonical registry surface it depends on.
    let names: Vec<String> = get_all_framework_resolvers()
        .iter()
        .map(|r| r.name().to_string())
        .collect();
    assert_eq!(names.len(), 28);
    assert_eq!(names[0], "laravel");
    assert!(names.contains(&"express".to_string()));
    assert!(names.contains(&"django".to_string()));
    assert!(names.contains(&"fabric-view".to_string()));
    assert!(names.contains(&"salesforce".to_string()));
}

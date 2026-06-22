use super::imports::strip_js_comments;
use super::normalize::{join_posix, normalize_segments, posix_dirname, relative_posix};
use super::*;
use crate::resolution::types::ReExport;
use crate::types::Language;

#[test]
fn posix_path_helpers_match_node_semantics() {
    assert_eq!(posix_dirname("src/components/Button.ts"), "src/components");
    assert_eq!(posix_dirname("main.c"), ".");
    assert_eq!(posix_dirname("/a"), "/");
    assert_eq!(join_posix("", "main.c"), "main.c");
    assert_eq!(join_posix("/tmp/x", "src/a.ts"), "/tmp/x/src/a.ts");
    assert_eq!(
        normalize_segments("src/components/../helpers"),
        "src/helpers"
    );
    assert_eq!(normalize_segments("./../x"), "../x");
    assert_eq!(normalize_segments("/tmp/p/../../x"), "/x");
    assert_eq!(relative_posix("", "src/utils"), "src/utils");
    assert_eq!(relative_posix("/tmp/p", "/tmp/p/src/a"), "src/a");
    assert_eq!(relative_posix("/tmp/p", "/x"), "../../x");
    assert_eq!(relative_posix("/tmp/p", "/tmp/p"), "");
}

#[test]
fn strip_js_comments_preserves_strings() {
    let src = "const a = \"// not a comment\"; // real comment\n/* block */ const b = 'x';";
    let out = strip_js_comments(src);
    assert!(out.contains("\"// not a comment\""));
    assert!(!out.contains("real comment"));
    assert!(!out.contains("block"));
    assert!(out.contains("const b = 'x';"));
}

#[test]
fn extract_re_exports_recognises_all_forms() {
    let content = r#"
export { foo } from './a';
export { foo as bar } from './b';
export * from './c';
export * as ns from './d';
export { default as Foo } from './e';
// export { ghost } from './nope';
"#;
    let out = extract_re_exports(content, Language::Typescript);
    assert_eq!(
        out,
        vec![
            ReExport::Wildcard {
                source: "./c".into()
            },
            ReExport::Wildcard {
                source: "./d".into()
            },
            ReExport::Named {
                exported_name: "foo".into(),
                original_name: "foo".into(),
                source: "./a".into(),
            },
            ReExport::Named {
                exported_name: "bar".into(),
                original_name: "foo".into(),
                source: "./b".into(),
            },
            ReExport::Named {
                exported_name: "Foo".into(),
                original_name: "default".into(),
                source: "./e".into(),
            },
        ]
    );
}

#[test]
fn extract_re_exports_non_js_languages_return_empty() {
    assert!(extract_re_exports("export * from './x';", Language::Python).is_empty());
    assert!(extract_re_exports("export * from './x';", Language::Go).is_empty());
}

#[test]
fn java_import_mappings_carry_fqn_and_skip_wildcards() {
    let content = r#"
package com.example.app;

// import com.example.Commented;
import com.example.dao.FooConverter;
import static com.example.util.Strings.join;
import com.example.everything.*;

public class App {}
"#;
    let mappings = extract_import_mappings("App.java", content, Language::Java);
    assert_eq!(mappings.len(), 2);
    assert_eq!(mappings[0].local_name, "FooConverter");
    assert_eq!(mappings[0].exported_name, "FooConverter");
    assert_eq!(mappings[0].source, "com.example.dao.FooConverter");
    assert_eq!(mappings[1].local_name, "join");
    assert_eq!(mappings[1].source, "com.example.util.Strings.join");
}

#[test]
fn go_import_mappings_single_and_block() {
    let content = r#"
package main

import "fmt"
import alias "github.com/example/proj/pkga"

import (
    "strings"
    p2 "github.com/example/proj/pkgb"
)
"#;
    let mappings = extract_import_mappings("main.go", content, Language::Go);
    let names: Vec<(&str, &str)> = mappings
        .iter()
        .map(|m| (m.local_name.as_str(), m.source.as_str()))
        .collect();
    assert!(names.contains(&("fmt", "fmt")));
    assert!(names.contains(&("alias", "github.com/example/proj/pkga")));
    assert!(names.contains(&("strings", "strings")));
    assert!(names.contains(&("p2", "github.com/example/proj/pkgb")));
    assert!(
        mappings
            .iter()
            .all(|m| m.is_namespace && m.exported_name == "*")
    );
}

#[test]
fn php_use_statements_with_alias() {
    let content = "<?php\nuse App\\Models\\User;\nuse App\\Services\\Auth as AuthService;\n";
    let mappings = extract_import_mappings("a.php", content, Language::Php);
    assert_eq!(mappings.len(), 2);
    assert_eq!(mappings[0].local_name, "User");
    assert_eq!(mappings[0].source, "App\\Models\\User");
    assert_eq!(mappings[1].local_name, "AuthService");
    assert_eq!(mappings[1].exported_name, "Auth");
}

#[test]
fn js_require_statements() {
    let content = "const fs = require('fs');\nconst { a, b: c } = require('./lib');\n";
    let mappings = extract_import_mappings("a.js", content, Language::Javascript);
    assert_eq!(mappings.len(), 3);
    assert!(mappings[0].is_default && mappings[0].local_name == "fs");
    assert_eq!(mappings[1].local_name, "a");
    assert_eq!(mappings[2].local_name, "c");
    assert_eq!(mappings[2].exported_name, "b");
}

#[test]
fn svelte_and_vue_reuse_js_import_extraction() {
    let content = "<script>\nimport Button from './Button.svelte';\n</script>\n<div/>";
    for lang in [Language::Svelte, Language::Vue] {
        let mappings = extract_import_mappings("App.svelte", content, lang);
        assert_eq!(mappings.len(), 1, "{lang:?}");
        assert_eq!(mappings[0].local_name, "Button");
        assert!(mappings[0].is_default);
    }
}

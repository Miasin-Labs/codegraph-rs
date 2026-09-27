use super::context::{is_js_family_path, is_low_value_js_ts_resolution_source};
use super::policy::{
    APEX_BUILT_IN_METHODS,
    APEX_SYSTEM_TYPES,
    BASH_BUILT_INS,
    C_BUILT_INS,
    CPP_BUILT_INS,
    GO_BUILT_INS,
    GO_STDLIB_PACKAGES,
    JS_BUILT_INS,
    PASCAL_BUILT_INS,
    PASCAL_UNIT_PREFIXES,
    PYTHON_BUILT_IN_METHODS,
    PYTHON_BUILT_IN_TYPES,
    PYTHON_BUILT_INS,
    REACT_HOOKS,
    capitalize_first,
};
use crate::resolution::types::UnresolvedRef;
use crate::types::{EdgeKind, Language};

#[test]
fn capitalize_first_matches_js() {
    assert_eq!(capitalize_first("recorder"), "Recorder");
    assert_eq!(capitalize_first("Recorder"), "Recorder");
    assert_eq!(capitalize_first(""), "");
    assert_eq!(capitalize_first("a"), "A");
}

#[test]
fn js_family_regex_matches_ts_pattern() {
    for path in [
        "a.ts",
        "a.tsx",
        "a.js",
        "a.jsx",
        "a.mts",
        "a.cts",
        "a.mjs",
        "a.cjs",
        "a.d.ts",
        "DIR/B.TSX",
    ] {
        assert!(is_js_family_path(path), "{path} should be JS-family");
    }
    for path in ["a.svelte", "a.vue", "a.py", "a.tsx.bak", "ats"] {
        assert!(!is_js_family_path(path), "{path} should NOT be JS-family");
    }
}

#[test]
fn low_value_js_ts_resolution_sources_are_skipped() {
    let reference = |file_path: &str, language: Language| UnresolvedRef {
        from_node_id: "node:src/app.js:caller:1".to_string(),
        reference_name: "_get".to_string(),
        reference_kind: EdgeKind::Calls,
        line: 1,
        column: 1,
        file_path: file_path.to_string(),
        language,
        candidates: None,
        metadata: None,
    };

    assert!(is_low_value_js_ts_resolution_source(&reference(
        "assets/jquery-ui-1.11.4.min.js",
        Language::Javascript,
    )));
    assert!(is_low_value_js_ts_resolution_source(&reference(
        "assets/app.min.tsx",
        Language::Tsx,
    )));
    assert!(is_low_value_js_ts_resolution_source(&reference(
        "deobfuscated-bundles/bundle-1.deob.js",
        Language::Javascript,
    )));
    assert!(is_low_value_js_ts_resolution_source(&reference(
        "tmp/bundle-1.deob.js",
        Language::Javascript,
    )));
    assert!(!is_low_value_js_ts_resolution_source(&reference(
        "src/minifier.js",
        Language::Javascript,
    )));
    assert!(!is_low_value_js_ts_resolution_source(&reference(
        "src/runtime-bundle.js",
        Language::Javascript,
    )));
    assert!(!is_low_value_js_ts_resolution_source(&reference(
        "assets/site.min.css",
        Language::Javascript,
    )));
    assert!(!is_low_value_js_ts_resolution_source(&reference(
        "assets/tool.min.js",
        Language::Python,
    )));
}

#[test]
fn built_in_sets_have_ts_cardinalities() {
    assert_eq!(JS_BUILT_INS.len(), 28);
    assert_eq!(REACT_HOOKS.len(), 10);
    assert_eq!(PYTHON_BUILT_INS.len(), 23);
    assert_eq!(PYTHON_BUILT_IN_TYPES.len(), 13);
    assert_eq!(PYTHON_BUILT_IN_METHODS.len(), 45);
    assert_eq!(GO_STDLIB_PACKAGES.len(), 67);
    assert_eq!(GO_BUILT_INS.len(), 40);
    assert_eq!(PASCAL_UNIT_PREFIXES.len(), 15);
    assert_eq!(PASCAL_BUILT_INS.len(), 87);
    assert_eq!(C_BUILT_INS.len(), 137);
    assert_eq!(CPP_BUILT_INS.len(), 25);
    assert_eq!(APEX_SYSTEM_TYPES.len(), 46);
    assert_eq!(APEX_BUILT_IN_METHODS.len(), 42);
    assert_eq!(BASH_BUILT_INS.len(), 103);
}

#[test]
fn apex_built_in_sets_are_lowercase() {
    for value in APEX_SYSTEM_TYPES.iter().chain(APEX_BUILT_IN_METHODS.iter()) {
        assert_eq!(
            *value,
            value.to_lowercase(),
            "entry {value:?} must be lowercase"
        );
    }
}

mod resolver_context {
    //! The production [`ResolverContext`]'s name lookups and file reads,
    //! over a real index and project directory.

    use std::sync::Arc;

    use tempfile::{TempDir, tempdir};

    use crate::db::{DatabaseConnection, QueryBuilder};
    use crate::resolution::resolver::ResolverContext;
    use crate::resolution::types::ResolutionContext;
    use crate::types::{Language, Node, NodeKind};

    fn rust_node(kind: NodeKind, name: &str, qualified: &str, file: &str) -> Node {
        Node::new(
            format!("{}:{file}:{qualified}", kind.as_str()),
            kind,
            name,
            qualified,
            file,
            Language::Rust,
            1,
            2,
        )
    }

    /// A context over an index holding `nodes`, rooted at a fresh project
    /// directory (the returned guards keep both alive).
    fn context_with(nodes: &[Node]) -> (ResolverContext, TempDir, DatabaseConnection) {
        let project = tempdir().expect("project dir");
        let connection = DatabaseConnection::initialize(project.path().join("codegraph.db"))
            .expect("index database");
        let queries = QueryBuilder::new(connection.get_db().expect("database handle"));
        queries.insert_nodes(nodes).expect("insert nodes");
        let root = project.path().to_string_lossy().into_owned();
        (ResolverContext::new(root, queries), project, connection)
    }

    fn ids(mut nodes: Vec<Node>) -> Vec<String> {
        let mut ids: Vec<String> = nodes.drain(..).map(|node| node.id).collect();
        ids.sort();
        ids
    }

    #[test]
    fn get_nodes_by_name_returns_exactly_the_same_named_nodes() {
        let nodes = [
            rust_node(NodeKind::Struct, "Graph", "Graph", "src/a.rs"),
            rust_node(NodeKind::Struct, "Graph", "Graph", "src/b.rs"),
            rust_node(NodeKind::Method, "graph", "Store::graph", "src/c.rs"),
            rust_node(NodeKind::Struct, "GraphSet", "GraphSet", "src/d.rs"),
        ];
        let (context, _project, _connection) = context_with(&nodes);
        let expected = vec![nodes[0].id.clone(), nodes[1].id.clone()];
        assert_eq!(ids(context.get_nodes_by_name("Graph")), expected);
        // Asked again, the (cached) answer is the same.
        assert_eq!(ids(context.get_nodes_by_name("Graph")), expected);
        // Case matters, and a prefix is not a match.
        assert_eq!(
            ids(context.get_nodes_by_name("graph")),
            vec![nodes[2].id.clone()]
        );
        assert!(context.get_nodes_by_name("Gra").is_empty());
        assert!(context.get_nodes_by_name("Missing").is_empty());
    }

    #[test]
    fn cached_file_text_reads_a_project_file_once() {
        let (context, project, _connection) = context_with(&[]);
        std::fs::create_dir_all(project.path().join("src")).expect("src dir");
        let path = project.path().join("src/lib.rs");
        std::fs::write(&path, "pub fn one() {}\n").expect("write source");

        let first = context.read_file_arc("src/lib.rs").expect("file text");
        assert_eq!(&*first, "pub fn one() {}\n");
        // The second read is the cached text, not the file re-read.
        std::fs::write(&path, "pub fn two() {}\n").expect("rewrite source");
        let second = context.read_file_arc("src/lib.rs").expect("cached text");
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(
            context.read_file("src/lib.rs").as_deref(),
            Some("pub fn one() {}\n")
        );
    }

    #[test]
    fn cached_file_text_is_none_for_a_missing_file_and_stays_none() {
        let (context, project, _connection) = context_with(&[]);
        assert_eq!(context.read_file_arc("src/missing.rs"), None);
        // The miss is cached too: a file created afterwards is not seen.
        std::fs::create_dir_all(project.path().join("src")).expect("src dir");
        std::fs::write(project.path().join("src/missing.rs"), "fn f() {}\n").expect("write source");
        assert_eq!(context.read_file("src/missing.rs"), None);
    }

    /// Invalid UTF-8 is read lossily rather than dropped.
    #[test]
    fn cached_file_text_reads_invalid_utf8_lossily() {
        let (context, project, _connection) = context_with(&[]);
        std::fs::write(project.path().join("bytes.rs"), b"ok \xFF end").expect("write");
        assert_eq!(
            context.read_file_arc("bytes.rs").as_deref(),
            Some("ok \u{FFFD} end")
        );
    }
}

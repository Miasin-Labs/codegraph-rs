use super::{Fixture, make_ref, match_by_qualified_name, match_reference, node};
use crate::resolution::types::ResolvedBy;
use crate::types::{EdgeKind, Language, NodeKind};

// -- "should match qualified name references" ----------------------------
#[test]
fn matches_qualified_name_references() {
    let class_node = node(
        "class:user.ts:User:5",
        NodeKind::Class,
        "User",
        "user.ts::User",
        "user.ts",
        Language::Typescript,
        5,
        30,
    );
    let method_node = node(
        "method:user.ts:User.save:15",
        NodeKind::Method,
        "save",
        "user.ts::User::save",
        "user.ts",
        Language::Typescript,
        15,
        25,
    );
    let ctx = Fixture::new(vec![class_node, method_node]);

    let r = make_ref(
        "User.save",
        EdgeKind::Calls,
        5,
        "main.ts",
        Language::Typescript,
    );
    let result = match_reference(&r, &ctx).expect("should resolve");

    assert_eq!(result.target_node_id, "method:user.ts:User.save:15");
}

// -- Rust-side coverage: partial qualified-name suffix match --------------
#[test]
fn qualified_name_partial_suffix_match() {
    let method_node = node(
        "method:src/user.ts:User::save:15",
        NodeKind::Method,
        "save",
        "src/user.ts::User::save",
        "src/user.ts",
        Language::Typescript,
        15,
        25,
    );
    let ctx = Fixture::new(vec![method_node]);
    let r = make_ref(
        "User::save",
        EdgeKind::Calls,
        5,
        "main.ts",
        Language::Typescript,
    );
    let result = match_by_qualified_name(&r, &ctx).expect("should resolve");
    assert_eq!(result.target_node_id, "method:src/user.ts:User::save:15");
    assert_eq!(result.confidence, 0.85);
    assert_eq!(result.resolved_by, ResolvedBy::QualifiedName);
}

// The suffix must start at a name boundary: `Value::from` is not
// `TomlValue::from`, and `Map::new` is not `OrderedNodeMap::new`.
#[test]
fn qualified_name_partial_suffix_match_requires_a_name_boundary() {
    let toml_from = node(
        "method:src/toml.rs:TomlValue::from:7",
        NodeKind::Method,
        "from",
        "TomlValue::from",
        "src/toml.rs",
        Language::Rust,
        7,
        9,
    );
    let ctx = Fixture::new(vec![toml_from]);
    for (reference, language) in [
        ("Value::from", Language::Rust),
        ("Map::new", Language::Rust),
        ("Helper.calc", Language::Typescript),
    ] {
        let r = make_ref(reference, EdgeKind::Calls, 3, "src/main.rs", language);
        assert!(
            match_by_qualified_name(&r, &ctx).is_none(),
            "{reference} must not match a longer name ending in it"
        );
    }
    let r = make_ref(
        "TomlValue::from",
        EdgeKind::Calls,
        3,
        "src/main.rs",
        Language::Rust,
    );
    assert!(match_by_qualified_name(&r, &ctx).is_some());
}

// Two modules each define a test `Ctx::new`: a reference picks the one in
// its own file, and one from a third file is left unresolved, not guessed.
#[test]
fn qualified_name_partial_match_prefers_the_referencing_file() {
    let ctx_in = |file: &str| {
        node(
            &format!("method:{file}:Ctx::new:9"),
            NodeKind::Method,
            "new",
            "Ctx::new",
            file,
            Language::Rust,
            9,
            12,
        )
    };
    let fixture = Fixture::new(vec![ctx_in("src/a.rs"), ctx_in("src/b.rs")]);
    for file in ["src/a.rs", "src/b.rs"] {
        let r = make_ref("Ctx::new", EdgeKind::Calls, 20, file, Language::Rust);
        let resolved = match_by_qualified_name(&r, &fixture).expect("same-file Ctx");
        assert_eq!(resolved.target_node_id, format!("method:{file}:Ctx::new:9"));
    }
    let r = make_ref("Ctx::new", EdgeKind::Calls, 20, "src/c.rs", Language::Rust);
    assert!(match_by_qualified_name(&r, &fixture).is_none());
}

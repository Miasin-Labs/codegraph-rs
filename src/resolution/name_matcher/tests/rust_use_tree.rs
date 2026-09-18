use crate::resolution::name_matcher::{
    UseBinding,
    UseLeaf,
    UseVisibility,
    parse_use_leaves,
    rust_use_leaves,
};
use crate::types::{Language, NodeKind};

/// `(visibility, path, binding)` with the path as `a::b` text and the binding
/// as `name`, `mod name` (a `{self}` import), or `*`.
type Expected<'a> = (UseVisibility, &'a str, &'a str);

fn leaf((vis, path, binding): Expected<'_>) -> UseLeaf {
    let path = if path.is_empty() {
        Vec::new()
    } else {
        path.split("::").map(str::to_string).collect()
    };
    let binding = match binding {
        "*" => UseBinding::Glob,
        name => match name.strip_prefix("mod ") {
            Some(module) => UseBinding::Module(module.to_string()),
            None => UseBinding::Name(name.to_string()),
        },
    };
    UseLeaf { vis, path, binding }
}

#[test]
fn parses_use_declarations_into_leaves() {
    use UseVisibility::{Crate, Private, Public, Restricted, Super};
    let restricted = |path: &str| Restricted(path.split("::").map(str::to_string).collect());
    let cases: Vec<(&str, Vec<Expected<'_>>)> = vec![
        ("use a::b;", vec![(Private, "a::b", "b")]),
        (
            "pub use catalog::tools;",
            vec![(Public, "catalog::tools", "tools")],
        ),
        (
            "pub(crate) use a::{b, c as d};",
            vec![(Crate, "a::b", "b"), (Crate, "a::c", "d")],
        ),
        (
            "pub(in crate::mcp::tools) use budget::{\n    adaptive, // why\n    now_ms,\n};",
            vec![
                (
                    restricted("crate::mcp::tools"),
                    "budget::adaptive",
                    "adaptive",
                ),
                (restricted("crate::mcp::tools"), "budget::now_ms", "now_ms"),
            ],
        ),
        ("pub(super) use x::*;", vec![(Super, "x", "*")]),
        ("pub(self) use a::b;", vec![(Private, "a::b", "b")]),
        (
            "use a::{self, b::{c, d::*}};",
            vec![
                (Private, "a", "mod a"),
                (Private, "a::b::c", "c"),
                (Private, "a::b::d", "*"),
            ],
        ),
        (
            "pub use a::b::{self as m};",
            vec![(Public, "a::b", "mod m")],
        ),
        ("use a::Trait as _;", vec![]),
        ("use a::{Trait as _, b};", vec![(Private, "a::b", "b")]),
        ("use ::std::fmt;", vec![]),
        ("use ::std::{fmt, io};", vec![]),
        ("extern crate alloc;", vec![]),
        (
            "use {a::b, c};",
            vec![(Private, "a::b", "b"), (Private, "c", "c")],
        ),
        ("use r#type::r#fn;", vec![(Private, "type::fn", "fn")]),
        (
            "use crate::a::{b /* inline */, /* nested /* x */ */ c};",
            vec![(Private, "crate::a::b", "b"), (Private, "crate::a::c", "c")],
        ),
        ("use super::*;", vec![(Private, "super", "*")]),
        (
            "use self::inner::f;",
            vec![(Private, "self::inner::f", "f")],
        ),
        ("use $crate::m::f;", vec![(Private, "$crate::m::f", "f")]),
        ("use a::{};", vec![]),
        ("use a::{b,,c", vec![(Private, "a::b", "b")]),
        ("", vec![]),
    ];
    for (source, expected) in cases {
        let expected: Vec<UseLeaf> = expected.into_iter().map(leaf).collect();
        assert_eq!(parse_use_leaves(source), expected, "{source:?}");
    }
}

#[test]
fn deeply_nested_use_lists_do_not_exhaust_the_stack() {
    let depth = 20_000;
    let source = format!("use {}x{};", "a::{".repeat(depth), "}".repeat(depth));
    let leaves = parse_use_leaves(&source);
    assert_eq!(leaves.len(), 1);
    assert_eq!(leaves[0].path.len(), depth + 1);
}

#[test]
fn file_leaves_carry_their_inline_module() {
    let mut top = super::node(
        "top",
        NodeKind::Import,
        "catalog",
        "catalog",
        "src/registry.rs",
        Language::Rust,
        1,
        1,
    );
    top.signature = Some("pub use catalog::tools;".into());
    let mut nested = super::node(
        "nested",
        NodeKind::Import,
        "super",
        "tests::super",
        "src/registry.rs",
        Language::Rust,
        9,
        9,
    );
    nested.signature = Some("use super::*;".into());
    let not_rust = super::node(
        "py",
        NodeKind::Import,
        "os",
        "os",
        "src/registry.rs",
        Language::Python,
        1,
        1,
    );
    let uses = rust_use_leaves([&top, &nested, &not_rust]);
    assert_eq!(uses.len(), 2);
    assert!(uses[0].inline_modules.is_empty());
    assert_eq!(uses[0].leaf.bound_name(), Some("tools"));
    assert_eq!(uses[1].inline_modules, ["tests"]);
    assert_eq!(uses[1].leaf.binding, UseBinding::Glob);
}

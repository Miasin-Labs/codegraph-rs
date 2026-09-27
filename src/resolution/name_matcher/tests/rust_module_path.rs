//! Paths through a module in scope: `m::f()` after `mod m;` or `use
//! crate::m;`, `self::m::f()`, `m::Type::new()`, and re-exports on the way.

use super::rust_path::{func, import, method, module, rust_ref, with_visibility};
use super::{Fixture, match_reference, node};
use crate::resolution::types::UnresolvedRef;
use crate::types::{Language, Node, NodeKind, Visibility};

/// The target a reference resolves to, if any.
fn target(ctx: &Fixture, reference: &UnresolvedRef) -> Option<String> {
    match_reference(reference, ctx).map(|hit| hit.target_node_id)
}

/// `mod <qualified>` declared in `file` (`a::m` for an inline `mod a { mod
/// m; }`).
fn module_at(id: &str, qualified: &str, file: &str) -> Node {
    let name = qualified.rsplit("::").next().unwrap_or(qualified);
    node(
        id,
        NodeKind::Module,
        name,
        qualified,
        file,
        Language::Rust,
        1,
        1,
    )
}

/// `lib.rs` declares `mod m;` (`src/m.rs`) and `mod other;`, both with an
/// `f`, plus a `Thing::new` in each.
fn two_modules() -> Vec<Node> {
    vec![
        module("m-mod", "m", "src/lib.rs"),
        module("other-mod", "other", "src/lib.rs"),
        func("m-f", "f", "f", "src/m.rs"),
        func("other-f", "f", "f", "src/other.rs"),
        method("m-new", "new", "Thing::new", "src/m.rs"),
        method("other-new", "new", "Thing::new", "src/other.rs"),
    ]
}

#[test]
fn a_child_module_path_reaches_the_module_file() {
    let mut nodes = two_modules();
    nodes.push(func("entry", "entry", "entry", "src/lib.rs"));
    let ctx = Fixture::new(nodes);
    for path in ["m::f", "self::m::f"] {
        let r = rust_ref("entry", path, "src/lib.rs");
        let hit = match_reference(&r, &ctx).expect(path);
        assert_eq!(hit.target_node_id, "m-f", "{path}");
        assert_eq!(hit.confidence, 0.95, "{path}");
    }
    let r = rust_ref("entry", "other::f", "src/lib.rs");
    assert_eq!(target(&ctx, &r).as_deref(), Some("other-f"));
}

#[test]
fn a_child_module_of_a_nested_module_uses_its_mod_rs() {
    // `src/a.rs` declares `mod m;`, whose items are in `src/a/m/mod.rs`.
    let ctx = Fixture::new(vec![
        module("a-mod", "a", "src/lib.rs"),
        module("m-mod", "m", "src/a.rs"),
        func("caller", "run", "run", "src/a.rs"),
        func("m-f", "f", "f", "src/a/m/mod.rs"),
        func("root-m-f", "f", "f", "src/m.rs"),
    ]);
    let r = rust_ref("caller", "m::f", "src/a.rs");
    assert_eq!(target(&ctx, &r).as_deref(), Some("m-f"));
}

#[test]
fn an_inline_module_path_reaches_its_items() {
    // `mod inl { pub fn g() {} pub struct T; impl T { fn new() } }` in lib.rs.
    let ctx = Fixture::new(vec![
        func("entry", "entry", "entry", "src/lib.rs"),
        module_at("inl-mod", "inl", "src/lib.rs"),
        func("inl-g", "g", "inl::g", "src/lib.rs"),
        method("inl-new", "new", "inl::T::new", "src/lib.rs"),
        func("decoy-g", "g", "g", "src/other.rs"),
        method("decoy-new", "new", "T::new", "src/other.rs"),
    ]);
    for (path, wanted) in [
        ("inl::g", "inl-g"),
        ("self::inl::g", "inl-g"),
        ("crate::inl::g", "inl-g"),
        ("inl::T::new", "inl-new"),
    ] {
        let r = rust_ref("entry", path, "src/lib.rs");
        assert_eq!(target(&ctx, &r).as_deref(), Some(wanted), "{path}");
    }
}

#[test]
fn a_used_module_resolves_through_its_use() {
    // `use crate::png;` then `png::get_exif_attr(..)` (kamadak-exif), next
    // to `jpeg::get_exif_attr`.
    let ctx = Fixture::new(vec![
        module("png-mod", "png", "src/lib.rs"),
        module("jpeg-mod", "jpeg", "src/lib.rs"),
        module("reader-mod", "reader", "src/lib.rs"),
        import("use-png", "crate", "src/reader.rs", "use crate::png;"),
        import("use-jpeg", "crate", "src/reader.rs", "use crate::jpeg;"),
        func("read", "read_raw", "Reader::read_raw", "src/reader.rs"),
        func("png-get", "get_exif_attr", "get_exif_attr", "src/png.rs"),
        func("jpeg-get", "get_exif_attr", "get_exif_attr", "src/jpeg.rs"),
    ]);
    let r = rust_ref("read", "png::get_exif_attr", "src/reader.rs");
    assert_eq!(target(&ctx, &r).as_deref(), Some("png-get"));
    let r = rust_ref("read", "jpeg::get_exif_attr", "src/reader.rs");
    assert_eq!(target(&ctx, &r).as_deref(), Some("jpeg-get"));
}

#[test]
fn nested_self_and_renamed_uses_bind_the_module() {
    let ctx = Fixture::new(vec![
        module("a-mod", "a", "src/lib.rs"),
        module("m-mod", "m", "src/a.rs"),
        module("user-mod", "user", "src/lib.rs"),
        func("m-f", "f", "f", "src/a/m.rs"),
        func("decoy-f", "f", "f", "src/m.rs"),
        method("m-new", "new", "Thing::new", "src/a/m.rs"),
        method("decoy-new", "new", "Thing::new", "src/b.rs"),
        // src/user.rs
        import("use-a-m", "crate", "src/user.rs", "use crate::a::m;"),
        import("use-n", "crate", "src/user.rs", "use crate::a::m as n;"),
        func("user", "user", "user", "src/user.rs"),
        // src/a.rs: `use self::m as own;`
        import("use-own", "self", "src/a.rs", "use self::m as own;"),
        func("a-run", "run", "run", "src/a.rs"),
    ]);
    for (from, file, path, wanted) in [
        ("user", "src/user.rs", "m::f", "m-f"),
        ("user", "src/user.rs", "n::f", "m-f"),
        ("user", "src/user.rs", "m::Thing::new", "m-new"),
        ("user", "src/user.rs", "n::Thing::new", "m-new"),
        ("a-run", "src/a.rs", "own::f", "m-f"),
        ("a-run", "src/a.rs", "self::m::f", "m-f"),
    ] {
        let r = rust_ref(from, path, file);
        assert_eq!(
            target(&ctx, &r).as_deref(),
            Some(wanted),
            "{path} in {file}"
        );
    }
}

#[test]
fn a_glob_brings_the_parents_modules_into_an_inline_test_module() {
    // `mod tests { use super::*; fn case() { m::f(); } }` in lib.rs.
    let mut nodes = two_modules();
    nodes.push(module_at("tests-mod", "tests", "src/lib.rs"));
    nodes.push(import(
        "glob",
        "tests::super",
        "src/lib.rs",
        "use super::*;",
    ));
    nodes.push(func("case", "case", "tests::case", "src/lib.rs"));
    let ctx = Fixture::new(nodes);
    let r = rust_ref("case", "m::f", "src/lib.rs");
    assert_eq!(target(&ctx, &r).as_deref(), Some("m-f"));
}

/// The File node the index keeps for every indexed file.
fn file(path: &str) -> Node {
    let name = path.rsplit('/').next().unwrap_or(path);
    node(
        &format!("file:{path}"),
        NodeKind::File,
        name,
        path,
        path,
        Language::Rust,
        1,
        1,
    )
}

#[test]
fn a_module_declared_inside_a_macro_call_is_found_by_its_file() {
    // tokio: `cfg_rt! { mod blocking; }` in runtime/mod.rs leaves no Module
    // node, but runtime/blocking/mod.rs is indexed.
    let ctx = Fixture::new(vec![
        module("runtime-mod", "runtime", "src/lib.rs"),
        file("src/runtime/mod.rs"),
        file("src/runtime/blocking/mod.rs"),
        file("src/runtime/coop.rs"),
        func(
            "pool",
            "create_blocking_pool",
            "create_blocking_pool",
            "src/runtime/blocking/mod.rs",
        ),
        func("stop", "stop", "stop", "src/runtime/coop.rs"),
        func("decoy-stop", "stop", "stop", "src/signal/mod.rs"),
        import(
            "use-blocking",
            "crate",
            "src/runtime/builder.rs",
            "use crate::runtime::{blocking, driver};",
        ),
        func("build", "build", "Builder::build", "src/runtime/builder.rs"),
        func("enter", "enter", "enter", "src/runtime/mod.rs"),
    ]);
    let r = rust_ref(
        "build",
        "blocking::create_blocking_pool",
        "src/runtime/builder.rs",
    );
    assert_eq!(target(&ctx, &r).as_deref(), Some("pool"));
    // A child of the caller's own module, called without a `use`.
    let r = rust_ref("enter", "coop::stop", "src/runtime/mod.rs");
    assert_eq!(target(&ctx, &r).as_deref(), Some("stop"));
    // No such file under the caller: not a module of the project.
    let r = rust_ref("build", "coop::stop", "src/runtime/builder.rs");
    assert_eq!(super::super::match_rust_path(&r, &ctx), None);
}

#[test]
fn a_glob_reaches_a_renamed_macro_declared_module() {
    // tokio's loom tests: runtime/tests/mod.rs declares `mod loom_oneshot;`
    // inside a macro call; loom_multi_thread.rs says `use
    // crate::runtime::tests::loom_oneshot as oneshot;`, and its `mod
    // group_b { use super::*; … oneshot::channel() }`. Another fn of the
    // file says `use crate::sync::oneshot;`, which binds only there.
    let file_path = "src/runtime/tests/loom_multi_thread.rs";
    let source = (1..=20)
        .map(|line| match line {
            15 => "        use crate::sync::oneshot;".to_string(),
            _ => String::new(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    let span = |mut node: Node, start: u32, end: u32| {
        node.start_line = start;
        node.end_line = end;
        node
    };
    let mut ctx = Fixture::new(vec![
        module("runtime-mod", "runtime", "src/lib.rs"),
        module("tests-mod", "tests", "src/runtime/mod.rs"),
        module("sync-mod", "sync", "src/lib.rs"),
        module("oneshot-mod", "oneshot", "src/sync/mod.rs"),
        file("src/runtime/tests/mod.rs"),
        file("src/runtime/tests/loom_oneshot.rs"),
        func(
            "loom-channel",
            "channel",
            "channel",
            "src/runtime/tests/loom_oneshot.rs",
        ),
        func("sync-channel", "channel", "channel", "src/sync/oneshot.rs"),
        import(
            "rename",
            "crate",
            file_path,
            "use crate::runtime::tests::loom_oneshot as oneshot;",
        ),
        module_at("group-mod", "group_b", file_path),
        import("glob", "group_b::super", file_path, "use super::*;"),
        span(func("case", "case", "group_b::case", file_path), 5, 10),
        span(func("other", "other", "group_b::other", file_path), 12, 18),
    ]);
    ctx.files.insert(file_path.into(), source);
    let at = |from: &str, line: u32| {
        let mut r = rust_ref(from, "oneshot::channel", file_path);
        r.line = line;
        r
    };
    assert_eq!(
        target(&ctx, &at("case", 7)).as_deref(),
        Some("loom-channel")
    );
    assert_eq!(
        target(&ctx, &at("other", 16)).as_deref(),
        Some("sync-channel")
    );
}

#[test]
fn a_type_path_under_a_module_reaches_the_associated_fn() {
    let mut nodes = two_modules();
    nodes.push(func("entry", "entry", "entry", "src/lib.rs"));
    let ctx = Fixture::new(nodes);
    let r = rust_ref("entry", "m::Thing::new", "src/lib.rs");
    assert_eq!(target(&ctx, &r).as_deref(), Some("m-new"));
}

#[test]
fn a_crate_visible_reexport_is_followed() {
    // rulex: lib.rs `mod parse; pub(crate) use parse::parse;`, and
    // parse/mod.rs `mod parsers; pub(crate) use parsers::parse;`.
    let ctx = Fixture::new(vec![
        module("parse-mod", "parse", "src/lib.rs"),
        import(
            "root-use",
            "parse",
            "src/lib.rs",
            "pub(crate) use parse::parse;",
        ),
        module("parsers-mod", "parsers", "src/parse/mod.rs"),
        import(
            "parse-use",
            "parsers",
            "src/parse/mod.rs",
            "pub(crate) use parsers::parse;",
        ),
        func("parse-fn", "parse", "parse", "src/parse/parsers.rs"),
        func("decoy", "parse", "parse", "src/options.rs"),
        func("entry", "entry", "Rulex::parse_and_compile", "src/lib.rs"),
        func("compile", "compile", "compile", "src/compile.rs"),
    ]);
    for (from, file, path) in [
        ("entry", "src/lib.rs", "parse::parse"),
        ("entry", "src/lib.rs", "self::parse::parse"),
        ("compile", "src/compile.rs", "crate::parse"),
        ("compile", "src/compile.rs", "crate::parse::parse"),
    ] {
        let r = rust_ref(from, path, file);
        assert_eq!(
            target(&ctx, &r).as_deref(),
            Some("parse-fn"),
            "{path} in {file}"
        );
    }
}

#[test]
fn a_module_out_of_scope_is_not_guessed() {
    // `m` is a child of `a` and of `b`; `src/c.rs` brings neither in.
    let ctx = Fixture::new(vec![
        module("a-mod", "a", "src/lib.rs"),
        module("b-mod", "b", "src/lib.rs"),
        module("am-mod", "m", "src/a.rs"),
        module("bm-mod", "m", "src/b.rs"),
        func("a-f", "f", "f", "src/a/m.rs"),
        func("b-f", "f", "f", "src/b/m.rs"),
        func("caller", "run", "run", "src/c.rs"),
    ]);
    let r = rust_ref("caller", "m::f", "src/c.rs");
    assert_eq!(super::super::match_rust_path(&r, &ctx), None);
    assert_eq!(target(&ctx, &r), None);
}

#[test]
fn a_module_two_globs_supply_is_ambiguous() {
    // `use crate::a::*; use crate::b::*;` where both declare `pub mod m`.
    let ctx = Fixture::new(vec![
        module("a-mod", "a", "src/lib.rs"),
        module("b-mod", "b", "src/lib.rs"),
        with_visibility(module("am-mod", "m", "src/a.rs"), Visibility::Public),
        with_visibility(module("bm-mod", "m", "src/b.rs"), Visibility::Public),
        func("a-f", "f", "f", "src/a/m.rs"),
        func("b-f", "f", "f", "src/b/m.rs"),
        import("glob-a", "crate", "src/c.rs", "use crate::a::*;"),
        import("glob-b", "crate", "src/c.rs", "use crate::b::*;"),
        func("caller", "run", "run", "src/c.rs"),
    ]);
    let r = rust_ref("caller", "m::f", "src/c.rs");
    assert_eq!(super::super::match_rust_path(&r, &ctx), None);
    assert_eq!(target(&ctx, &r), None);
    // One glob alone is enough.
    let ctx = Fixture::new(vec![
        module("a-mod", "a", "src/lib.rs"),
        with_visibility(module("am-mod", "m", "src/a.rs"), Visibility::Public),
        func("a-f", "f", "f", "src/a/m.rs"),
        func("b-f", "f", "f", "src/b/m.rs"),
        import("glob-a", "crate", "src/c.rs", "use crate::a::*;"),
        func("caller", "run", "run", "src/c.rs"),
    ]);
    assert_eq!(target(&ctx, &r).as_deref(), Some("a-f"));
}

#[test]
fn a_private_item_is_not_reached_from_outside_its_module() {
    // `m::helper` from src/user.rs, where `m`'s `helper` has no `pub`: the
    // path must name something the index lacks (a generated item).
    let ctx = Fixture::new(vec![
        module("m-mod", "m", "src/lib.rs"),
        import("use-m", "crate", "src/user.rs", "use crate::m;"),
        func("user", "user", "user", "src/user.rs"),
        with_visibility(
            func("helper", "helper", "helper", "src/m.rs"),
            Visibility::Private,
        ),
        with_visibility(
            func("inner", "inner", "inner", "src/m.rs"),
            Visibility::Public,
        ),
        // `m`'s own test module may call it.
        func("m-case", "case", "tests::case", "src/m.rs"),
    ]);
    let r = rust_ref("user", "m::helper", "src/user.rs");
    assert_eq!(super::super::match_rust_path(&r, &ctx), None);
    assert_eq!(target(&ctx, &r), None);
    let r = rust_ref("user", "m::inner", "src/user.rs");
    assert_eq!(target(&ctx, &r).as_deref(), Some("inner"));
    let r = rust_ref("m-case", "super::helper", "src/m.rs");
    assert_eq!(target(&ctx, &r).as_deref(), Some("helper"));
    let r = rust_ref("m-case", "crate::m::helper", "src/m.rs");
    assert_eq!(target(&ctx, &r).as_deref(), Some("helper"));
}

/// A project `fmt` module (`src/fmt.rs`, declared in lib.rs) with a
/// `format` fn, and a `user` module.
fn project_fmt() -> Vec<Node> {
    vec![
        module("fmt-mod", "fmt", "src/lib.rs"),
        module("user-mod", "user", "src/lib.rs"),
        func("fmt-format", "format", "format", "src/fmt.rs"),
        func("entry", "entry", "entry", "src/lib.rs"),
        func("user", "user", "user", "src/user.rs"),
    ]
}

#[test]
fn a_std_path_stays_off_a_same_named_project_module() {
    // src/user.rs: `use std::fmt;` then `fmt::format(..)`.
    let mut nodes = project_fmt();
    nodes.push(import("use-fmt", "std", "src/user.rs", "use std::fmt;"));
    let ctx = Fixture::new(nodes);
    let r = rust_ref("user", "fmt::format", "src/user.rs");
    assert_eq!(super::super::match_rust_path(&r, &ctx), None);
    assert_eq!(target(&ctx, &r), None);

    // Without any `use`, `fmt` is not in scope in `user` either.
    let ctx = Fixture::new(project_fmt());
    assert_eq!(super::super::match_rust_path(&r, &ctx), None);

    // In lib.rs, which declares `mod fmt;`, it is the project's.
    let r = rust_ref("entry", "fmt::format", "src/lib.rs");
    assert_eq!(target(&ctx, &r).as_deref(), Some("fmt-format"));
}

#[test]
fn a_fn_local_use_binds_only_inside_its_fn() {
    // lib.rs declares `mod fmt;`; `show` says `use std::fmt;` (and so means
    // std's), `entry` does not (and so means the project's). `pick` uses a
    // project module under another name.
    let source = "mod fmt;\n\
                  fn show() {\n\
                  \x20   use std::fmt;\n\
                  \x20   fmt::format(format_args!(\"\"));\n\
                  }\n\
                  fn entry() {\n\
                  \x20   fmt::format(1);\n\
                  }\n\
                  fn pick() {\n\
                  \x20   use crate::fmt as text;\n\
                  \x20   text::format(2);\n\
                  }\n";
    let span = |mut node: Node, start: u32, end: u32| {
        node.start_line = start;
        node.end_line = end;
        node
    };
    let mut ctx = Fixture::new(vec![
        module("fmt-mod", "fmt", "src/lib.rs"),
        func("fmt-format", "format", "format", "src/fmt.rs"),
        span(func("show", "show", "show", "src/lib.rs"), 2, 5),
        span(func("entry", "entry", "entry", "src/lib.rs"), 6, 8),
        span(func("pick", "pick", "pick", "src/lib.rs"), 9, 12),
    ]);
    ctx.files.insert("src/lib.rs".into(), source.into());
    let at = |from: &str, path: &str, line: u32| {
        let mut r = rust_ref(from, path, "src/lib.rs");
        r.line = line;
        r
    };
    assert_eq!(
        super::super::match_rust_path(&at("show", "fmt::format", 4), &ctx),
        None
    );
    assert_eq!(
        target(&ctx, &at("entry", "fmt::format", 7)).as_deref(),
        Some("fmt-format")
    );
    assert_eq!(
        target(&ctx, &at("pick", "text::format", 11)).as_deref(),
        Some("fmt-format")
    );
    // `text` binds nowhere else.
    assert_eq!(
        super::super::match_rust_path(&at("entry", "text::format", 7), &ctx),
        None
    );
}

#[test]
fn std_and_type_heads_are_left_to_other_rules() {
    let mut nodes = two_modules();
    nodes.push(func("entry", "entry", "entry", "src/lib.rs"));
    nodes.push(import("use-mem", "std", "src/lib.rs", "use std::mem;"));
    nodes.push(func("take", "take", "take", "src/m.rs"));
    let ctx = Fixture::new(nodes);
    for path in ["mem::take", "std::mem::take", "u32::from", "Thing::new"] {
        let r = rust_ref("entry", path, "src/lib.rs");
        assert_eq!(super::super::match_rust_path(&r, &ctx), None, "{path}");
    }
}

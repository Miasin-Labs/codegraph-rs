//! Miri output samples (checked in under `samples/`: real `cargo miri
//! test` output, and Miri's own UI-test expectations with `LL:CC` places)
//! read back, and test selection over a small call graph.

use std::path::Path;

use super::diagnostic::{Category, UbKind, parse_output};
use super::run::{Borrows, MiriOptions, RunStatus, status_of};
use super::select::{TestFn, TestTarget, select_tests};
use crate::analyze::bugs::{CallSite, FnSpan, Project};
use crate::analyze::fuzz::graph::CallGraph;

fn sample(name: &str) -> &'static str {
    match name {
        "use_after_free" => include_str!("samples/use_after_free.stderr"),
        "out_of_bounds" => include_str!("samples/out_of_bounds.stderr"),
        "uninit_read" => include_str!("samples/uninit_read.stderr"),
        "invalid_value" => include_str!("samples/invalid_value.stderr"),
        "stacked_borrows" => include_str!("samples/stacked_borrows.stderr"),
        "tree_borrows" => include_str!("samples/tree_borrows.stderr"),
        "memory_leak" => include_str!("samples/memory_leak.stderr"),
        "unsupported_ffi" => include_str!("samples/unsupported_ffi.stderr"),
        "unsupported_isolation" => include_str!("samples/unsupported_isolation.stderr"),
        "data_race" => include_str!("samples/data_race.stderr"),
        "alignment" => include_str!("samples/alignment.stderr"),
        _ => panic!("no sample {name}"),
    }
}

#[test]
fn every_sample_names_its_kind_and_status() {
    let expected = [
        ("use_after_free", UbKind::UseAfterFree, RunStatus::Ub),
        ("out_of_bounds", UbKind::OutOfBounds, RunStatus::Ub),
        ("uninit_read", UbKind::UninitRead, RunStatus::Ub),
        ("invalid_value", UbKind::InvalidValue, RunStatus::Ub),
        ("stacked_borrows", UbKind::StackedBorrows, RunStatus::Ub),
        ("tree_borrows", UbKind::TreeBorrows, RunStatus::Ub),
        ("memory_leak", UbKind::MemoryLeak, RunStatus::Leak),
        (
            "unsupported_ffi",
            UbKind::Unsupported,
            RunStatus::Unsupported,
        ),
        (
            "unsupported_isolation",
            UbKind::Unsupported,
            RunStatus::Unsupported,
        ),
        ("data_race", UbKind::DataRace, RunStatus::Ub),
        ("alignment", UbKind::Alignment, RunStatus::Ub),
    ];
    for (name, kind, status) in expected {
        let parsed = parse_output(sample(name));
        let diagnostic = parsed
            .primary()
            .unwrap_or_else(|| panic!("{name}: no diagnostic"));
        assert_eq!(diagnostic.kind, kind, "{name}: {diagnostic:?}");
        assert_eq!(status_of(&parsed, false, false, false), status, "{name}");
        assert_eq!(diagnostic.kind.rule(), format!("miri::{}", kind.id()));
    }
}

#[test]
fn a_use_after_free_has_its_span_frames_and_allocation_history() {
    let parsed = parse_output(sample("use_after_free"));
    let d = parsed.primary().unwrap();
    assert_eq!(d.category, Category::UndefinedBehavior);
    assert_eq!(
        d.message,
        "memory access failed: ALLOC has been freed, so this pointer is dangling"
    );
    let primary = d.primary.as_ref().unwrap();
    assert_eq!(
        (primary.file.as_str(), primary.line, primary.col),
        ("src/lib.rs", 15, 14)
    );
    let frames: Vec<(&str, u32)> = d
        .frames
        .iter()
        .map(|f| (f.function.as_str(), f.place.line))
        .collect();
    assert_eq!(
        frames,
        [
            ("uaf", 15),
            ("tests::t_uaf", 57),
            ("tests::t_uaf::{closure#0}", 57)
        ]
    );
    let related: Vec<(&str, u32)> = d
        .related
        .iter()
        .map(|r| (r.note.as_str(), r.place.line))
        .collect();
    assert_eq!(
        related,
        [
            ("ALLOC was allocated here", 12),
            ("ALLOC was deallocated here", 14)
        ]
    );
    assert_eq!(d.thread.as_deref(), Some("tests::t_uaf"));
}

#[test]
fn borrow_violations_name_their_model_and_history() {
    let stacked = parse_output(sample("stacked_borrows"));
    let d = stacked.primary().unwrap();
    assert!(
        d.message.contains("<TAG>"),
        "tags are normalized: {}",
        d.message
    );
    assert_eq!(d.primary.as_ref().unwrap().line, 23);
    assert_eq!(d.related.len(), 2, "{:?}", d.related);
    assert!(d.related[1].note.contains("later invalidated"));

    let tree = parse_output(sample("tree_borrows"));
    let d = tree.primary().unwrap();
    assert_eq!(d.primary.as_ref().unwrap().line, 24);
    assert!(
        d.related[1].note.contains("transitioned to Disabled"),
        "{:?}",
        d.related
    );
}

#[test]
fn a_leak_is_reported_after_the_test_passed() {
    let parsed = parse_output(sample("memory_leak"));
    assert_eq!(parsed.passed, 1);
    let d = parsed.primary().unwrap();
    assert_eq!(d.category, Category::MemoryLeak);
    assert_eq!(d.primary.as_ref().unwrap().line, 29);
    assert_eq!(d.frames[0].function, "leak");
}

#[test]
fn unsupported_operations_keep_miris_reason_and_hint() {
    let ffi = parse_output(sample("unsupported_ffi"));
    let d = ffi.primary().unwrap();
    assert_eq!(d.category, Category::Unsupported);
    assert!(!d.category.is_proof());
    assert_eq!(
        d.message,
        "can't call foreign function `my_missing_c_fn` on OS `linux`"
    );
    assert_eq!(d.frames[0].function, "ffi");

    let isolated = parse_output(sample("unsupported_isolation"));
    let d = isolated.primary().unwrap();
    assert!(d.message.contains("isolation is enabled"), "{}", d.message);
    assert!(
        d.help
            .iter()
            .any(|h| h.contains("-Zmiri-disable-isolation")),
        "{:?}",
        d.help
    );
    // Normalized UI-test places read as line 0.
    assert_eq!(d.primary.as_ref().unwrap().line, 0);
}

#[test]
fn a_thread_race_keeps_the_earlier_access() {
    let parsed = parse_output(sample("data_race"));
    let d = parsed.primary().unwrap();
    assert!(
        d.related
            .iter()
            .any(|r| r.note.contains("(1) occurred earlier here"))
    );
    assert!(
        d.related
            .iter()
            .any(|r| r.note.contains("got called indirectly")),
        "{:?}",
        d.related
    );
}

#[test]
fn runs_without_a_diagnostic_read_from_libtest() {
    let clean = "\nrunning 1 test\ntest tests::fine ... ok\n\ntest result: ok. 1 passed; 0 \
                 failed; 0 ignored; 0 measured; 8 filtered out; finished in 0.18s\n\nrunning 0 \
                 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered \
                 out\n";
    let parsed = parse_output(clean);
    assert!(parsed.ran_tests);
    assert_eq!(status_of(&parsed, true, false, false), RunStatus::Clean);

    let none = "running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; \
                3 filtered out\n";
    assert_eq!(
        status_of(&parse_output(none), true, false, false),
        RunStatus::NoTest
    );
    let ignored = "running 1 test\ntest t ... ignored\n\ntest result: ok. 0 passed; 0 failed; \
                   1 ignored; 0 measured; 0 filtered out\n";
    assert_eq!(
        status_of(&parse_output(ignored), true, false, false),
        RunStatus::Ignored
    );

    let panic = "running 1 test\ntest tests::t ... FAILED\n\nfailures:\n\n---- tests::t stdout \
                 ----\n\nthread 'tests::t' panicked at src/lib.rs:9:5:\nassertion `left == \
                 right` failed\n\nfailures:\n    tests::t\n\ntest result: FAILED. 0 passed; 1 \
                 failed; 0 ignored; 0 measured; 0 filtered out\n";
    let parsed = parse_output(panic);
    assert_eq!(
        status_of(&parsed, false, false, false),
        RunStatus::TestFailed
    );
    assert_eq!(parsed.panics[0].message, "assertion `left == right` failed");
    assert_eq!(parsed.panics[0].place.as_ref().unwrap().line, 9);
    assert_eq!(parsed.failed_tests, ["tests::t"]);

    assert_eq!(
        status_of(&parse_output(""), false, true, false),
        RunStatus::Timeout
    );
    assert_eq!(
        status_of(&parse_output(""), false, false, true),
        RunStatus::OutputCapped
    );
    assert!(RunStatus::Timeout.is_inconclusive() && !RunStatus::Clean.is_inconclusive());
}

#[test]
fn miriflags_follow_the_options() {
    let default = MiriOptions::default();
    assert_eq!(default.miriflags(), "-Zmiri-strict-provenance");
    let tree = MiriOptions {
        borrows: Borrows::Tree,
        disable_isolation: true,
        strict_provenance: false,
        extra_flags: vec!["-Zmiri-seed=7".into()],
        ..MiriOptions::default()
    };
    assert_eq!(
        tree.miriflags(),
        "-Zmiri-tree-borrows -Zmiri-disable-isolation -Zmiri-seed=7"
    );
}

fn span(id: &str, name: &str, file: &str, line: u32, is_test: bool) -> FnSpan {
    FnSpan {
        id: id.into(),
        name: name.into(),
        qualified_name: name.into(),
        kind: "function".into(),
        file: file.into(),
        start_line: line,
        end_line: line + 3,
        start_col: 0,
        signature: None,
        return_type: None,
        is_test,
    }
}

fn call(from: &FnSpan, to: &FnSpan) -> CallSite {
    CallSite {
        caller_id: from.id.clone(),
        caller: from.qualified_name.clone(),
        file: from.file.clone(),
        line: from.start_line + 1,
        col: 4,
        callee_id: to.id.clone(),
        callee_name: to.name.clone(),
        callee_qualified: to.qualified_name.clone(),
        callee_kind: to.kind.clone(),
        callee_signature: None,
        callee_return_type: None,
        callee_file: to.file.clone(),
        callee_line: to.start_line,
        in_test: from.is_test,
    }
}

fn test_fn(graph: &CallGraph, id: &str, path: &str, skip: Option<&str>) -> TestFn {
    let index = graph.index_of(id).unwrap();
    let span = &graph.functions[index];
    TestFn {
        index,
        path: path.into(),
        file: span.file.clone(),
        line: span.start_line,
        crate_dir: String::new(),
        package: "demo".into(),
        target: TestTarget::Lib,
        skip: skip.map(str::to_string),
    }
}

/// `raw_read` (unsafe) ← `parse` ← `decode`; tests reach them at
/// different depths; `orphan` has no test.
#[test]
fn tests_are_chosen_nearest_first_covering_every_aimed_function() {
    let raw = span("raw", "raw_read", "src/lib.rs", 10, false);
    let parse = span("parse", "parse", "src/lib.rs", 20, false);
    let decode = span("decode", "decode", "src/lib.rs", 30, false);
    let orphan = span("orphan", "orphan", "src/lib.rs", 40, false);
    let other = span("other", "other_unsafe", "src/lib.rs", 50, false);
    let t_decode = span("t1", "tests::decodes", "src/lib.rs", 100, true);
    let t_parse = span("t2", "tests::parses", "src/lib.rs", 110, true);
    let t_ignored = span("t3", "tests::slow", "src/lib.rs", 120, true);
    let t_other = span("t4", "tests::other", "src/lib.rs", 130, true);
    let calls = vec![
        call(&parse, &raw),
        call(&decode, &parse),
        call(&t_decode, &decode),
        call(&t_parse, &parse),
        call(&t_ignored, &raw),
        call(&t_other, &other),
    ];
    let functions = vec![
        raw, parse, decode, orphan, other, t_decode, t_parse, t_ignored, t_other,
    ];
    let project = Project::from_parts(
        Path::new("/nonexistent"),
        vec!["src/lib.rs".into()],
        functions,
        calls,
    );
    let graph = CallGraph::new(&project);
    let tests = vec![
        test_fn(&graph, "t1", "tests::decodes", None),
        test_fn(&graph, "t2", "tests::parses", None),
        test_fn(&graph, "t3", "tests::slow", Some("marked #[ignore]")),
        test_fn(&graph, "t4", "tests::other", None),
    ];
    let aimed: Vec<usize> = ["raw", "orphan", "other"]
        .iter()
        .map(|id| graph.index_of(id).unwrap())
        .collect();

    let (selected, uncovered) = select_tests(&graph, &tests, &aimed, 10);
    let chosen: Vec<(&str, u32)> = selected
        .iter()
        .map(|s| (s.test.path.as_str(), s.distance))
        .collect();
    // The ignored test (1 hop) is never chosen; `parses` (2 hops) is the
    // nearest for raw_read, then `other` for other_unsafe, then the rest.
    assert_eq!(
        chosen,
        [
            ("tests::parses", 2),
            ("tests::other", 1),
            ("tests::decodes", 3)
        ]
    );
    assert_eq!(selected[0].reaches, ["raw_read"]);
    assert_eq!(uncovered, [graph.index_of("orphan").unwrap()]);

    // A cap keeps the covering tests first.
    let (capped, _) = select_tests(&graph, &tests, &aimed, 2);
    let names: Vec<&str> = capped.iter().map(|s| s.test.path.as_str()).collect();
    assert_eq!(names, ["tests::parses", "tests::other"]);
}

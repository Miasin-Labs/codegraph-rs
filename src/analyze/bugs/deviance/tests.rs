//! The templates on small fixtures, with the functions and resolved calls an
//! index would hold built by hand.

use std::path::Path;

use super::results::{Explicit, Use, reports_status};
use super::{companions, detect, scan};
use crate::analyze::bugs::{CallSite, Finding, FnSpan, Project};

fn span(id: &str, name: &str, file: &str, lines: (u32, u32), signature: &str) -> FnSpan {
    FnSpan {
        id: id.to_string(),
        name: name.to_string(),
        qualified_name: format!("S::{name}"),
        kind: "method".to_string(),
        file: file.to_string(),
        start_line: lines.0,
        end_line: lines.1,
        start_col: 4,
        signature: Some(signature.to_string()),
        return_type: None,
        is_test: false,
    }
}

fn site(caller: &FnSpan, callee: &FnSpan, at: (u32, u32)) -> CallSite {
    CallSite {
        caller_id: caller.id.clone(),
        caller: caller.qualified_name.clone(),
        file: caller.file.clone(),
        line: at.0,
        col: at.1,
        callee_id: callee.id.clone(),
        callee_name: callee.name.clone(),
        callee_qualified: callee.qualified_name.clone(),
        callee_kind: callee.kind.clone(),
        callee_signature: callee.signature.clone(),
        callee_return_type: None,
        callee_file: callee.file.clone(),
        callee_line: callee.start_line,
        in_test: false,
    }
}

/// `(line, col)` of the `nth` occurrence of `needle` in `source`.
fn at(source: &str, needle: &str, nth: usize) -> (u32, u32) {
    let offset = source
        .match_indices(needle)
        .nth(nth)
        .unwrap_or_else(|| panic!("no {needle} #{nth}"))
        .0;
    let before = &source[..offset];
    let line = before.matches('\n').count() as u32 + 1;
    let col = (offset - before.rfind('\n').map_or(0, |nl| nl + 1)) as u32;
    (line, col)
}

/// The line holding the first `needle`.
fn line_of(source: &str, needle: &str) -> u32 {
    at(source, needle, 0).0
}

/// The KeepBoth shape from a real sync engine: two arms return what the
/// project call did; `Both` downloads a copy and yields `None`, so the
/// caller never records that the conflict was settled.
const SYNC: &str = r#"impl S {
    fn upload(&mut self, p: &str) -> Option<u32> {
        Some(1)
    }
    fn download(&mut self, p: &str) -> Option<u32> {
        Some(2)
    }
    fn fetch(&mut self, p: &str) -> Result<u32, ()> {
        Ok(3)
    }
    fn sync(&mut self, r: Res, out: &mut Vec<u32>) -> Option<u32> {
        match r {
            Res::Local if out.is_empty() => self.upload("a"),
            Res::Cloud => self.download("b").map(|n| n + 1),
            Res::Both { renamed } => {
                match self.fetch(renamed) {
                    Ok(_) => out.push(1),
                    Err(_) => {}
                }
                BOTH_TAIL
            }
            Res::Manual => {
                out.push(2);
                None
            }
            _ => None,
        }
    }
    fn one(&mut self) -> u32 {
        let n = self.fetch("1").unwrap();
        n
    }
    fn two(&mut self) -> Option<u32> {
        Some(self.fetch("2").ok()?)
    }
    fn three(&mut self) -> Result<u32, ()> {
        self.fetch("3")
    }
}
"#;

/// The fixture's project, with `tail` ending the `Both` arm.
fn sync_project(dir: &Path, tail: &str) -> Project {
    let source = SYNC.replace("BOTH_TAIL", tail);
    std::fs::write(dir.join("sync.rs"), &source).unwrap();
    let f = "sync.rs";
    let lines = |name: &str| {
        let start = line_of(&source, &format!("fn {name}("));
        let end = source
            .lines()
            .enumerate()
            .skip(start as usize)
            .find(|(_, line)| *line == "    }")
            .map(|(i, _)| i as u32 + 1)
            .unwrap();
        (start, end)
    };
    let upload = span(
        "u",
        "upload",
        f,
        lines("upload"),
        "(&mut self, p: &str) -> Option<u32>",
    );
    let download = span(
        "d",
        "download",
        f,
        lines("download"),
        "(&mut self, p: &str) -> Option<u32>",
    );
    let fetch = span(
        "f",
        "fetch",
        f,
        lines("fetch"),
        "(&mut self, p: &str) -> Result<u32, ()>",
    );
    let sync = span(
        "s",
        "sync",
        f,
        lines("sync"),
        "(&mut self, r: Res, out: &mut Vec<u32>) -> Option<u32>",
    );
    let one = span("1", "one", f, lines("one"), "(&mut self) -> u32");
    let two = span("2", "two", f, lines("two"), "(&mut self) -> Option<u32>");
    let three = span(
        "3",
        "three",
        f,
        lines("three"),
        "(&mut self) -> Result<u32, ()>",
    );
    let mut calls = vec![
        site(&sync, &upload, at(&source, "self.upload(\"a\")", 0)),
        site(&sync, &download, at(&source, "self.download(\"b\")", 0)),
        site(&sync, &fetch, at(&source, "self.fetch(renamed)", 0)),
        site(&one, &fetch, at(&source, "self.fetch(\"1\")", 0)),
        site(&two, &fetch, at(&source, "self.fetch(\"2\")", 0)),
        site(&three, &fetch, at(&source, "self.fetch(\"3\")", 0)),
    ];
    if tail.contains("self.upload(\"c\")") {
        calls.push(site(&sync, &upload, at(&source, "self.upload(\"c\")", 0)));
    }
    Project::from_parts(
        dir,
        vec![f.to_string()],
        vec![upload, download, fetch, sync, one, two, three],
        calls,
    )
}

fn by_rule<'f>(findings: &'f [Finding], rule: &str) -> Vec<&'f Finding> {
    findings.iter().filter(|f| f.rule == rule).collect()
}

#[test]
fn arm_that_drops_its_work_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let mut project = sync_project(dir.path(), "None");
    let findings = detect(&mut project);
    let source = SYNC.replace("BOTH_TAIL", "None");

    let arms = by_rule(&findings, "arm-result-deviance");
    assert_eq!(arms.len(), 1, "{findings:#?}");
    let arm = arms[0];
    assert_eq!(arm.line, line_of(&source, "Res::Both"));
    assert_eq!(arm.function.as_deref(), Some("S::sync"));
    assert!(arm.message.contains("`fetch`"), "{}", arm.message);
    assert!(arm.message.contains("`None`"), "{}", arm.message);
    // The sibling arms, as evidence.
    let lines: Vec<u32> = arm.evidence.iter().map(|e| e.line).collect();
    assert!(lines.contains(&line_of(&source, "Res::Local")));
    assert!(lines.contains(&line_of(&source, "Res::Cloud")));
    // `fetch`'s result is used at its three other sites and dropped by the
    // `Ok(_)` match here: merged into the arm finding, not reported twice.
    assert!(
        arm.evidence
            .iter()
            .any(|e| e.note.starts_with("result-discarded")),
        "{:#?}",
        arm.evidence
    );
    assert!(
        by_rule(&findings, "result-discarded").is_empty(),
        "{findings:#?}"
    );
    assert!(arm.confidence > 0.75, "{}", arm.confidence);
    // `Res::Manual` makes no project call; `_` is the catch-all.
    assert!(
        findings
            .iter()
            .all(|f| f.line != line_of(&source, "Res::Manual")
                && f.line != line_of(&source, "_ => None"))
    );
}

#[test]
fn arm_that_reports_its_work_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let mut project = sync_project(dir.path(), "self.upload(\"c\")");
    let findings = detect(&mut project);
    assert!(
        by_rule(&findings, "arm-result-deviance").is_empty(),
        "{findings:#?}"
    );
    // `match self.fetch(..) { Ok(_) => .., Err(_) => .. }` looks at the
    // outcome and drops the payload on purpose: without a deviant arm to
    // fold into, it is not reported on its own.
    assert!(
        by_rule(&findings, "result-discarded").is_empty(),
        "{findings:#?}"
    );
}

/// Each call context, classified.
#[test]
fn result_uses_are_classified_by_context() {
    let source = r#"fn used(s: &mut S) -> u32 {
    let a = s.get();
    take(s.get());
    if s.get() > 1 {}
    s.get().unwrap();
    let _: u32 = s.get();
    s.get()
}
fn discarded(s: &mut S) {
    s.get();
    let _ = s.get();
    _ = s.get();
    s.get().ok();
    if let Err(e) = s.get() {}
    match s.get() { Ok(_) => {}, Err(e) => {} }
    let ok = s.get().is_ok();
    s.get()?;
}
fn get(&mut self) -> Result<u32, E> { Ok(1) }
#[cfg(test)]
mod checks {
    fn check(s: &mut S) { s.get(); }
}
"#;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("uses.rs"), source).unwrap();
    let f = "uses.rs";
    let used = span("used", "used", f, (1, 8), "(s: &mut S) -> u32");
    let discarded = span("disc", "discarded", f, (9, 18), "(s: &mut S)");
    let get = span("get", "get", f, (19, 19), "(&mut self) -> Result<u32, E>");
    // In a `#[cfg(test)]` module the index does not name as one.
    let check = span("chk", "check", f, (22, 22), "(s: &mut S)");
    let mut calls = Vec::new();
    for nth in 0..source.matches("s.get()").count() {
        let pos = at(source, "s.get()", nth);
        let caller = match pos.0 {
            0..=8 => &used,
            9..=18 => &discarded,
            _ => &check,
        };
        calls.push(site(caller, &get, pos));
    }
    let mut project = Project::from_parts(
        dir.path(),
        vec![f.to_string()],
        vec![used, discarded, get, check],
        calls,
    );
    let scanned = scan(&mut project);
    let uses = scanned.uses;
    for (site, call) in project.call_sites().iter().enumerate() {
        if call.line > 19 {
            assert_eq!(uses[site], None, "test code teaches no belief");
            continue;
        }
        let expected = match call.line {
            ..=8 => Use::Used,
            10 => Use::Discarded,
            11 | 12 => Use::Handled(Explicit::Wildcard),
            13 => Use::Handled(Explicit::Silenced("ok")),
            14 | 15 => Use::Handled(Explicit::UnboundSuccess),
            16 => Use::Handled(Explicit::PayloadDropped("is_ok")),
            _ => Use::Checked,
        };
        assert_eq!(uses[site], Some(expected), "line {}", call.line);
    }
    assert_eq!(uses.iter().filter(|u| u.is_some()).count(), 14);
    // `s.get()?;` checks for failure: neither a use nor a discard.
    let checked = project
        .call_sites()
        .iter()
        .position(|call| call.line == 17)
        .unwrap();
    assert_eq!(uses[checked], Some(Use::Checked));
    assert_eq!(scanned.test_regions["uses.rs"], vec![(21, 23)]);
}

/// TS `switch`: the case that calls and returns `null` deviates.
#[test]
fn switch_case_returning_null_after_work_is_reported() {
    let source = r#"function route(kind: string): Item | null {
  switch (kind) {
    case "a":
      return loadA();
    case "b":
      return loadB();
    case "c":
      loadC();
      return null;
    default:
      return null;
  }
}
function loadA(): Item { return x; }
function loadB(): Item { return x; }
function loadC(): Item { return x; }
"#;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("route.ts"), source).unwrap();
    let f = "route.ts";
    let route = span("r", "route", f, (1, 13), "(kind: string): Item | null");
    let a = span("a", "loadA", f, (14, 14), "(): Item");
    let b = span("b", "loadB", f, (15, 15), "(): Item");
    let c = span("c", "loadC", f, (16, 16), "(): Item");
    let calls = vec![
        site(&route, &a, at(source, "loadA()", 0)),
        site(&route, &b, at(source, "loadB()", 0)),
        site(&route, &c, at(source, "loadC()", 0)),
    ];
    let mut project =
        Project::from_parts(dir.path(), vec![f.to_string()], vec![route, a, b, c], calls);
    let findings = detect(&mut project);
    let arms = by_rule(&findings, "arm-result-deviance");
    assert_eq!(arms.len(), 1, "{findings:#?}");
    assert_eq!(arms[0].line, 7);
}

#[test]
fn caller_missing_the_usual_companion_is_reported() {
    let f = "c.rs";
    let a = span("a", "begin", f, (1, 3), "()");
    let b = span("b", "commit", f, (4, 6), "()");
    let callers: Vec<FnSpan> = (0..6)
        .map(|i| {
            span(
                &format!("c{i}"),
                &format!("user{i}"),
                f,
                (10 + i * 10, 18 + i * 10),
                "()",
            )
        })
        .collect();
    // Filler, so `commit` (4 callers) is no ubiquitous utility.
    let filler: Vec<FnSpan> = (0..30)
        .map(|i| {
            span(
                &format!("x{i}"),
                &format!("other{i}"),
                f,
                (100 + i * 5, 103 + i * 5),
                "()",
            )
        })
        .collect();
    let mut calls = Vec::new();
    for (i, caller) in callers.iter().enumerate() {
        calls.push(site(caller, &a, (caller.start_line + 1, 4)));
        if i < 5 {
            calls.push(site(caller, &b, (caller.start_line + 2, 4)));
        }
    }
    let mut functions = vec![a, b];
    functions.extend(callers);
    functions.extend(filler);
    let project = Project::from_parts(
        Path::new("/nonexistent"),
        vec![f.to_string()],
        functions,
        calls,
    );
    let findings = companions::findings(&project, &Default::default(), &Default::default());
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert_eq!(findings[0].function.as_deref(), Some("S::user5"));
    assert!(
        findings[0].message.contains("`commit` (5 of 6)"),
        "{}",
        findings[0].message
    );
    assert_eq!(findings[0].evidence.len(), 5);
}

/// A project with one callee `get` (signature `ret`) called from `user`
/// once per line of `body` that contains `s.get(`.
fn one_callee_project(dir: &Path, ret: &str, body: &str) -> Project {
    let source =
        format!("fn user(s: &mut S) {{\n{body}}}\nfn get(&mut self) -> {ret} {{ todo!() }}\n");
    std::fs::write(dir.join("u.rs"), &source).unwrap();
    let f = "u.rs";
    let lines = source.lines().count() as u32;
    let user = span("user", "user", f, (1, lines - 1), "(s: &mut S)");
    let get = span(
        "get",
        "get",
        f,
        (lines, lines),
        &format!("(&mut self) -> {ret}"),
    );
    let calls = (0..source.matches("s.get(").count())
        .map(|nth| site(&user, &get, at(&source, "s.get(", nth)))
        .collect();
    Project::from_parts(dir, vec![f.to_string()], vec![user, get], calls)
}

const FOUR_USES: &str =
    "    let a = s.get();\n    take(s.get());\n    if s.get() {}\n    s.get().unwrap();\n";

/// RustSec false positives: `let _ = f()`, `if let Err(e) = f()`, `match
/// f() { Ok(_) => Ok(()), Err(e) => Err(e) }` are decisions, not unchecked
/// results; only a bare statement discard of a status result stands alone.
#[test]
fn explicit_discards_are_never_reported_alone() {
    for explicit in [
        "    let _ = s.get();\n",
        "    _ = s.get();\n",
        "    if let Err(e) = s.get() { return; }\n",
        "    match s.get() { Ok(_) => Ok(()), Err(e) => Err(e) }\n",
        "    let failed = s.get().is_err();\n",
        "    s.get().ok();\n",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let body = format!("{FOUR_USES}{explicit}");
        let mut project = one_callee_project(dir.path(), "Result<u32, E>", &body);
        let findings = detect(&mut project);
        assert!(
            by_rule(&findings, "result-discarded").is_empty(),
            "{explicit}: {findings:#?}"
        );
    }
    // The unchecked form is still a lead.
    let dir = tempfile::tempdir().unwrap();
    let body = format!("{FOUR_USES}    s.get();\n");
    let mut project = one_callee_project(dir.path(), "Result<u32, E>", &body);
    let findings = detect(&mut project);
    assert_eq!(
        by_rule(&findings, "result-discarded").len(),
        1,
        "{findings:#?}"
    );
}

/// `parser.add_child(..);` (`&'a AstNode`), `next(src);` advancing a cursor,
/// `with(|x| ..);` (a type parameter): results called for their side effect,
/// not status reports.
#[test]
fn statement_discard_of_a_non_status_result_is_not_reported() {
    for ret in [
        "&'a AstNode<'a>",
        "R",
        "Option<&'a ValRaw>",
        "Self",
        "impl Deref<Target = T>",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let body = format!("{FOUR_USES}    s.get();\n");
        let mut project = one_callee_project(dir.path(), ret, &body);
        let findings = detect(&mut project);
        assert!(
            by_rule(&findings, "result-discarded").is_empty(),
            "{ret}: {findings:#?}"
        );
    }
    for ret in [
        "bool",
        "io::Result<usize>",
        "LockResult<MutexGuard<'a, T>>",
        "Result<T, E>",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let body = format!("{FOUR_USES}    s.get();\n");
        let mut project = one_callee_project(dir.path(), ret, &body);
        let findings = detect(&mut project);
        assert_eq!(
            by_rule(&findings, "result-discarded").len(),
            1,
            "{ret}: {findings:#?}"
        );
    }
}

#[test]
fn status_results_are_result_like_or_boolean() {
    use crate::analyze::bugs::deviance::rules::for_language;
    use crate::types::Language;
    let rust = for_language(Language::Rust).unwrap();
    let ts = for_language(Language::Typescript).unwrap();
    assert!(reports_status(rust, "Result<u32>"));
    assert!(reports_status(rust, "std::io::Result<()>"));
    assert!(reports_status(rust, "&'a mut bool"));
    assert!(reports_status(rust, "SysCallResult"));
    assert!(!reports_status(rust, "Option<u32>"));
    assert!(!reports_status(rust, "R"));
    assert!(!reports_status(rust, "ErrorStack"));
    assert!(reports_status(ts, "Promise<boolean>"));
    assert!(!reports_status(ts, "Promise<Item>"));
}

/// Two arms yielding calls' results and the arms under test, in a function
/// returning `ret`.
fn arms_project(dir: &Path, ret: &str, arms: &str) -> (Project, String) {
    let source = format!(
        "fn run(s: &mut S, k: K) -> {ret} {{\n    match k {{\n        K::A => s.load(1),\n        \
         K::B => s.load(2),\n{arms}    }}\n}}\nfn load(&mut self, n: u8) -> {ret} {{ todo!() }}\n\
         fn step(&mut self, n: u8) -> Result<u32, E> {{ todo!() }}\n\
         fn name(&self) -> String {{ todo!() }}\n"
    );
    std::fs::write(dir.join("a.rs"), &source).unwrap();
    let f = "a.rs";
    let run_end = line_of(&source, "fn load(") - 1;
    let run = span(
        "run",
        "run",
        f,
        (1, run_end),
        &format!("(s: &mut S, k: K) -> {ret}"),
    );
    // `Result<T, E>` whatever `ret` is: a value (wasmtime's `unexpected` is
    // generic), so the sibling arms do work.
    let load = span(
        "load",
        "load",
        f,
        (run_end + 1, run_end + 1),
        "(&mut self, n: u8) -> Result<T, E>",
    );
    let step = span(
        "step",
        "step",
        f,
        (run_end + 2, run_end + 2),
        "(&mut self, n: u8) -> Result<u32, E>",
    );
    let name = span(
        "name",
        "name",
        f,
        (run_end + 3, run_end + 3),
        "(&self) -> String",
    );
    let mut calls = Vec::new();
    for (callee, needle) in [(&load, "s.load("), (&step, "s.step("), (&name, "s.name(")] {
        for nth in 0..source.matches(needle).count() {
            calls.push(site(&run, callee, at(&source, needle, nth)));
        }
    }
    (
        Project::from_parts(dir, vec![f.to_string()], vec![run, load, step, name], calls),
        source,
    )
}

/// wasmtime `Val::store`: in a `Result<()>` match, `Ok(())` after `?`-checked
/// work reports all a sibling could.
#[test]
fn arm_yielding_unit_success_is_not_deviant() {
    let dir = tempfile::tempdir().unwrap();
    let (mut project, _) = arms_project(
        dir.path(),
        "Result<()>",
        "        K::C => {\n            s.step(3)?;\n            Ok(())\n        }\n",
    );
    let findings = detect(&mut project);
    assert!(
        by_rule(&findings, "arm-result-deviance").is_empty(),
        "{findings:#?}"
    );
}

/// matrix-sdk `ForwardedRoomKeyContent::Unknown(_) => { warn!(..); Ok(None) }`:
/// the arm's call feeds a log line, so nothing it did was dropped. The same
/// arm dropping a call's result is the KeepBoth shape and is reported.
#[test]
fn arm_whose_calls_pass_their_value_on_is_not_deviant() {
    let dir = tempfile::tempdir().unwrap();
    let (mut project, _) = arms_project(
        dir.path(),
        "Result<Option<u32>, E>",
        "        K::C => {\n            let n = s.name();\n            log(n);\n            Ok(None)\n        }\n",
    );
    let findings = detect(&mut project);
    assert!(
        by_rule(&findings, "arm-result-deviance").is_empty(),
        "{findings:#?}"
    );

    let dir = tempfile::tempdir().unwrap();
    let (mut project, source) = arms_project(
        dir.path(),
        "Result<Option<u32>, E>",
        "        K::C => {\n            s.step(3)?;\n            Ok(None)\n        }\n",
    );
    let findings = detect(&mut project);
    let arms = by_rule(&findings, "arm-result-deviance");
    assert_eq!(arms.len(), 1, "{findings:#?}");
    assert_eq!(arms[0].line, line_of(&source, "K::C"));
}

#[test]
fn companion_pairs_name_an_acquire_and_its_release() {
    use super::companions::is_companion_pair;
    assert!(is_companion_pair("getDirContext", "closeDirContext"));
    assert!(is_companion_pair("lock", "unlock"));
    assert!(is_companion_pair(
        "pthread_mutex_lock",
        "pthread_mutex_unlock"
    ));
    assert!(is_companion_pair("begin_transaction", "commit"));
    assert!(is_companion_pair("transaction", "commit"));
    assert!(is_companion_pair("push_limit", "pop_limit"));
    assert!(is_companion_pair("open", "close"));
    // Unrelated helpers the co-occurrence belief paired on RustSec crates.
    assert!(!is_companion_pair("device_id", "user_id"));
    assert!(!is_companion_pair("raw", "new"));
    assert!(!is_companion_pair("hash_algo", "context"));
    assert!(!is_companion_pair("eof", "pop_limit"));
    assert!(!is_companion_pair("write", "open"));
    // A generic verb needs the same object: `getX` is not released by `close`.
    assert!(!is_companion_pair("getConnection", "close"));
    assert!(!is_companion_pair("get_device", "close_session"));
}

/// Callers of `a`, all but one also calling `b` (`support` of them).
fn companion_project(a: &str, b: &str, callers: usize) -> Vec<Finding> {
    let f = "c.rs";
    let a_span = span("a", a, f, (1, 3), "()");
    let b_span = span("b", b, f, (4, 6), "()");
    let users: Vec<FnSpan> = (0..callers)
        .map(|i| {
            let i = i as u32;
            span(
                &format!("c{i}"),
                &format!("user{i}"),
                f,
                (10 + i * 10, 18 + i * 10),
                "()",
            )
        })
        .collect();
    // Filler, so `b` is no ubiquitous utility.
    let filler: Vec<FnSpan> = (0..40)
        .map(|i| {
            span(
                &format!("x{i}"),
                &format!("other{i}"),
                f,
                (500 + i * 5, 503 + i * 5),
                "()",
            )
        })
        .collect();
    let mut calls = Vec::new();
    for (i, caller) in users.iter().enumerate() {
        calls.push(site(caller, &a_span, (caller.start_line + 1, 4)));
        if i + 1 < callers {
            calls.push(site(caller, &b_span, (caller.start_line + 2, 4)));
        }
    }
    let mut functions = vec![a_span, b_span];
    functions.extend(users);
    functions.extend(filler);
    let project = Project::from_parts(
        Path::new("/nonexistent"),
        vec![f.to_string()],
        functions,
        calls,
    );
    companions::findings(&project, &Default::default(), &Default::default())
}

/// OWASP `LDAPManager`: `getDirContext` without `closeDirContext` is kept;
/// the same statistics over unrelated names are not a belief; 4 of 5 is too
/// little support.
#[test]
fn companion_belief_needs_a_named_pair_and_support() {
    let found = companion_project("getDirContext", "closeDirContext", 8);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(
        found[0].message.contains("`closeDirContext` (7 of 8)"),
        "{}",
        found[0].message
    );
    assert!(companion_project("device_id", "user_id", 8).is_empty());
    assert!(companion_project("begin", "commit", 5).is_empty());
}

/// 3 of 4 is too little support for a discard to stand alone.
#[test]
fn a_lone_discard_needs_four_agreeing_sites() {
    let dir = tempfile::tempdir().unwrap();
    let body = "    let a = s.get();\n    take(s.get());\n    if s.get() {}\n    s.get();\n";
    let mut project = one_callee_project(dir.path(), "bool", body);
    let findings = detect(&mut project);
    assert!(
        by_rule(&findings, "result-discarded").is_empty(),
        "{findings:#?}"
    );
}

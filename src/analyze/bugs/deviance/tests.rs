//! The templates on small fixtures, with the functions and resolved calls an
//! index would hold built by hand.

use std::path::Path;

use super::results::Use;
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
    // The discarded `fetch` result stays a (standalone) lead.
    let discarded = by_rule(&findings, "result-discarded");
    assert_eq!(discarded.len(), 1, "{findings:#?}");
    assert!(
        discarded[0].message.contains("3 of 4"),
        "{}",
        discarded[0].message
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
        let expected_used = call.line <= 8;
        assert_eq!(
            uses[site] == Some(Use::Used),
            expected_used,
            "line {}: {:?}",
            call.line,
            uses[site]
        );
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
    let callers: Vec<FnSpan> = (0..5)
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
        if i < 4 {
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
    assert_eq!(findings[0].function.as_deref(), Some("S::user4"));
    assert!(
        findings[0].message.contains("`commit` (4 of 5)"),
        "{}",
        findings[0].message
    );
    assert_eq!(findings[0].evidence.len(), 4);
}

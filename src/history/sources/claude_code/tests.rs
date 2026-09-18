use std::collections::HashMap;
use std::io::Write as _;
use std::path::Path;

use super::*;
use crate::history::sources::Budget;

/// A root transcript: a human prompt, an injected command wrapper, a Read
/// and a failing `cargo check` answered by one result record, and a meta
/// line. Synthetic — no real session content.
fn root_lines(cwd: &str) -> Vec<String> {
    let ts = |s: u32| format!("2026-09-10T10:00:{s:02}.000Z");
    vec![
        r#"{"type":"file-history-snapshot","messageId":"m0"}"#.to_owned(),
        format!(
            r#"{{"type":"user","uuid":"u1","cwd":"{cwd}","timestamp":"{}","message":{{"role":"user","content":"fix the failing build in lib.rs"}}}}"#,
            ts(1)
        ),
        format!(
            r#"{{"type":"user","uuid":"u2","cwd":"{cwd}","timestamp":"{}","message":{{"role":"user","content":[{{"type":"text","text":"<command-name>/model</command-name>"}}]}}}}"#,
            ts(2)
        ),
        format!(
            r#"{{"type":"assistant","uuid":"a1","cwd":"{cwd}","timestamp":"{}","message":{{"role":"assistant","content":[{{"type":"text","text":"ok"}},{{"type":"tool_use","id":"toolu_R","name":"Read","input":{{"file_path":"{cwd}/src/lib.rs","offset":10}}}},{{"type":"tool_use","id":"toolu_B","name":"Bash","input":{{"command":"cargo check 2>&1 | head -50","description":"check"}}}}]}}}}"#,
            ts(3)
        ),
        format!(
            r#"{{"type":"user","uuid":"u3","cwd":"{cwd}","timestamp":"{}","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"toolu_R","content":"fn main() {{}}"}},{{"type":"tool_result","tool_use_id":"toolu_B","is_error":true,"content":"error[E0425]: cannot find value `x`"}}]}}}}"#,
            ts(4)
        ),
        format!(
            r#"{{"type":"user","uuid":"u4","isMeta":true,"cwd":"{cwd}","timestamp":"{}","message":{{"role":"user","content":"meta note"}}}}"#,
            ts(5)
        ),
    ]
}

fn sub_lines(cwd: &str) -> Vec<String> {
    vec![
        format!(
            r#"{{"type":"user","uuid":"s1","isSidechain":true,"cwd":"{cwd}","timestamp":"2026-09-10T10:00:06.000Z","message":{{"role":"user","content":"Investigate the thing"}}}}"#
        ),
        format!(
            r#"{{"type":"assistant","uuid":"s2","isSidechain":true,"cwd":"{cwd}","timestamp":"2026-09-10T10:00:07.000Z","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"toolu_G","name":"Grep","input":{{"pattern":"parse_config","path":"src"}}}}]}}}}"#
        ),
        format!(
            r#"{{"type":"user","uuid":"s3","isSidechain":true,"cwd":"{cwd}","timestamp":"2026-09-10T10:00:08.000Z","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"toolu_G","content":[{{"type":"text","text":"src/lib.rs:3"}}]}}]}}}}"#
        ),
    ]
}

fn write_lines(path: &Path, lines: &[String]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    for l in lines {
        writeln!(f, "{l}").unwrap();
    }
}

fn collect(
    src: &ClaudeCodeProjects,
    cursors: &HashMap<String, String>,
    scope: Option<&Path>,
) -> (Vec<SourceEvent>, SourceStats) {
    let budget = Budget::unlimited();
    let visit = Visit {
        cursors,
        budget: &budget,
        scope,
    };
    let mut events = Vec::new();
    let stats = src
        .visit_events(&visit, &mut |e| {
            events.push(e);
            Ok(())
        })
        .unwrap();
    (events, stats)
}

fn checkpoints(events: &[SourceEvent]) -> HashMap<String, String> {
    events
        .iter()
        .filter_map(|e| match e {
            SourceEvent::Checkpoint { key, value } => Some((key.clone(), value.clone())),
            _ => None,
        })
        .collect()
}

fn calls(events: &[SourceEvent]) -> Vec<&RawToolCall> {
    events
        .iter()
        .filter_map(|e| match e {
            SourceEvent::Call(c) => Some(&**c),
            _ => None,
        })
        .collect()
}

#[test]
fn transcripts_yield_sessions_prompts_and_joined_calls() {
    let tmp = tempfile::tempdir().unwrap();
    let projects = tmp.path().join("projects");
    let cwd = "/work/repo";
    write_lines(&projects.join("-work-repo/sess1.jsonl"), &root_lines(cwd));
    write_lines(
        &projects.join("-work-repo/sess1/subagents/agent-a1.jsonl"),
        &sub_lines(cwd),
    );
    // Workflow journals are not transcripts.
    write_lines(
        &projects.join("-work-repo/sess1/subagents/workflows/wf/journal.jsonl"),
        &[r#"{"type":"user","cwd":"/x","message":{"content":"no"}}"#.to_owned()],
    );
    let src = ClaudeCodeProjects::new(&projects);
    let (events, stats) = collect(&src, &HashMap::new(), None);
    assert_eq!((stats.inputs, stats.calls), (2, 3), "{events:#?}");

    // Root first: its session, the one human prompt, then its calls.
    let SourceEvent::Session(root) = &events[0] else {
        panic!("{events:#?}")
    };
    assert_eq!(
        (
            root.native_id.as_str(),
            root.parent.as_deref(),
            root.cwd.as_deref()
        ),
        ("sess1", None, Some(cwd))
    );
    let prompts: Vec<&RawPrompt> = events
        .iter()
        .filter_map(|e| match e {
            SourceEvent::Prompt(p) => Some(p),
            _ => None,
        })
        .collect();
    assert_eq!(
        prompts.len(),
        1,
        "wrappers, meta and sidechain turns are not prompts"
    );
    assert_eq!(prompts[0].native_id, "u1");

    let all = calls(&events);
    let read = all.iter().find(|c| c.native_id == "toolu_R").unwrap();
    assert_eq!(read.file_path.as_deref(), Some("/work/repo/src/lib.rs"));
    assert_eq!(read.line, Some(10));
    assert_eq!(read.result.as_ref().unwrap().output_bytes, 12);
    let bash = all.iter().find(|c| c.native_id == "toolu_B").unwrap();
    let result = bash.result.as_ref().unwrap();
    assert_eq!(result.is_error, Some(true));
    assert!(
        result.excerpt.as_deref().unwrap().contains("E0425"),
        "a build's output head is kept (in memory) for its error codes"
    );
    assert!(read.result.as_ref().unwrap().excerpt.is_none());

    // The sub-agent rolls up to its root.
    let sub = events
        .iter()
        .find_map(|e| match e {
            SourceEvent::Session(s) if s.parent.is_some() => Some(s),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        (sub.native_id.as_str(), sub.parent.as_deref()),
        ("sess1/agent-a1", Some("sess1"))
    );
    let grep = all.iter().find(|c| c.native_id == "toolu_G").unwrap();
    assert_eq!(grep.session.as_deref(), Some("sess1/agent-a1"));
    assert_eq!(grep.pattern.as_deref(), Some("parse_config"));
    assert_eq!(checkpoints(&events).len(), 2);

    // Checkpoints make the next pass a no-op.
    let (again, stats) = collect(&src, &checkpoints(&events), None);
    assert!(again.is_empty(), "{again:#?}");
    assert_eq!(stats.inputs, 0);
}

#[test]
fn an_unanswered_call_holds_the_checkpoint_until_its_result_lands() {
    let tmp = tempfile::tempdir().unwrap();
    let projects = tmp.path().join("projects");
    let path = projects.join("-r/s2.jsonl");
    let lines = root_lines("/r");
    // Everything up to the tool_use line, not its result.
    write_lines(&path, &lines[..4]);
    let src = ClaudeCodeProjects::new(&projects);
    let (first, _) = collect(&src, &HashMap::new(), None);
    assert!(calls(&first).is_empty(), "no result yet: {first:#?}");
    let cursor = checkpoints(&first);
    let offset: u64 = cursor.values().next().unwrap().parse().unwrap();
    let size = std::fs::metadata(&path).unwrap().len();
    assert!(
        offset < size,
        "checkpoint stays before the pending tool_use"
    );

    write_lines(&path, &lines[4..]);
    let (second, _) = collect(&src, &cursor, None);
    let got: Vec<&str> = calls(&second)
        .iter()
        .map(|c| c.native_id.as_str())
        .collect();
    assert_eq!(got, ["toolu_R", "toolu_B"]);
    let offset: u64 = checkpoints(&second)
        .values()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(offset, std::fs::metadata(&path).unwrap().len());
}

#[test]
fn out_of_scope_transcripts_are_left_unread() {
    let tmp = tempfile::tempdir().unwrap();
    let projects = tmp.path().join("projects");
    write_lines(&projects.join("-a/s.jsonl"), &root_lines("/elsewhere"));
    let src = ClaudeCodeProjects::new(&projects);
    let (events, _) = collect(&src, &HashMap::new(), Some(Path::new("/work")));
    assert!(events.is_empty(), "{events:#?}");
}

#[test]
fn a_spent_budget_defers_whole_transcripts() {
    let tmp = tempfile::tempdir().unwrap();
    let projects = tmp.path().join("projects");
    write_lines(&projects.join("-a/s1.jsonl"), &root_lines("/a"));
    write_lines(&projects.join("-b/s2.jsonl"), &root_lines("/b"));
    let src = ClaudeCodeProjects::new(&projects);
    let budget = Budget::new(None, Some(1));
    let cursors = HashMap::new();
    let visit = Visit {
        cursors: &cursors,
        budget: &budget,
        scope: None,
    };
    let mut n = 0;
    let stats = src
        .visit_events(&visit, &mut |e| {
            if matches!(e, SourceEvent::Call(_)) {
                budget.spend();
                n += 1;
            }
            Ok(())
        })
        .unwrap();
    assert_eq!((stats.inputs, stats.deferred, n), (1, 1, 2));
}

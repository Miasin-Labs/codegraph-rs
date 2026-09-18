use std::collections::HashMap;
use std::path::Path;

use rusqlite::Connection;
use serde_json::json;

use super::*;
use crate::history::sources::Budget;

/// A synthetic opencode database: the columns the adapter reads.
fn fixture(path: &Path) -> Connection {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE session (id TEXT PRIMARY KEY, parent_id TEXT, directory TEXT NOT NULL,
                               time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL);
         CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                               time_created INTEGER NOT NULL, data TEXT NOT NULL);
         CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT NOT NULL, session_id TEXT NOT NULL,
                            time_created INTEGER NOT NULL, data TEXT NOT NULL);
         CREATE INDEX part_session ON part (session_id, time_created, id);
         INSERT INTO session VALUES ('ses_root', NULL, '/work/repo', 1000, 9000);
         INSERT INTO session VALUES ('ses_child', 'ses_root', '/work/repo', 3000, 8000);
         INSERT INTO session VALUES ('ses_other', NULL, '/elsewhere', 1000, 2000);",
    )
    .unwrap();
    let msg = |id: &str, ses: &str, t: i64, role: &str| {
        conn.execute(
            "INSERT INTO message VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![id, ses, t, json!({ "role": role }).to_string()],
        )
        .unwrap();
    };
    let part = |id: &str, mid: &str, ses: &str, t: i64, data: serde_json::Value| {
        conn.execute(
            "INSERT INTO part VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, mid, ses, t, data.to_string()],
        )
        .unwrap();
    };
    msg("msg_u1", "ses_root", 1100, "user");
    part(
        "p1",
        "msg_u1",
        "ses_root",
        1100,
        json!({"type": "text", "text": "rename the parser module"}),
    );
    msg("msg_u2", "ses_root", 1200, "user");
    part(
        "p2",
        "msg_u2",
        "ses_root",
        1200,
        json!({"type": "text", "synthetic": true, "text": "continue"}),
    );
    msg("msg_a1", "ses_root", 1300, "assistant");
    let tool = |tool: &str,
                call: &str,
                status: &str,
                input: serde_json::Value,
                output: &str,
                exit: Option<i64>| {
        json!({"type": "tool", "tool": tool, "callID": call,
               "state": {"status": status, "input": input, "output": output,
                         "metadata": {"exit": exit}}})
    };
    part(
        "p3",
        "msg_a1",
        "ses_root",
        1310,
        tool(
            "read",
            "call_r",
            "completed",
            json!({"filePath": "/work/repo/src/parser.rs", "offset": 5}),
            "12345",
            None,
        ),
    );
    part(
        "p4",
        "msg_a1",
        "ses_root",
        1320,
        tool(
            "bash",
            "call_b",
            "completed",
            json!({"command": "cargo test -p parser", "workdir": "/work/repo"}),
            "error[E0433]: failed to resolve",
            Some(101),
        ),
    );
    part(
        "p5",
        "msg_a1",
        "ses_root",
        1330,
        tool(
            "apply_patch",
            "call_p",
            "completed",
            json!({"patchText": "*** Begin Patch\n*** Update File: src/parser.rs\n@@\n-a\n+b\n*** End Patch"}),
            "ok",
            None,
        ),
    );
    part(
        "p6",
        "msg_a1",
        "ses_root",
        1340,
        tool(
            "bash",
            "call_run",
            "running",
            json!({"command": "cargo build"}),
            "",
            None,
        ),
    );
    part(
        "p7",
        "msg_a1",
        "ses_root",
        1350,
        json!({"type": "reasoning", "text": "thinking"}),
    );
    msg("msg_c1", "ses_child", 3100, "user");
    part(
        "c1",
        "msg_c1",
        "ses_child",
        3100,
        json!({"type": "text", "text": "subtask prompt"}),
    );
    part(
        "c2",
        "msg_c1",
        "ses_child",
        3200,
        tool(
            "grep",
            "call_g",
            "completed",
            json!({"pattern": "fn parse_expr", "path": "/work/repo/src"}),
            "src/parser.rs:9",
            None,
        ),
    );
    conn
}

fn visit(
    src: &OpencodeDb,
    cursors: &HashMap<String, String>,
    scope: Option<&Path>,
) -> Vec<SourceEvent> {
    let budget = Budget::unlimited();
    let v = Visit {
        cursors,
        budget: &budget,
        scope,
    };
    let mut events = Vec::new();
    src.visit_events(&v, &mut |e| {
        events.push(e);
        Ok(())
    })
    .unwrap();
    events
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

fn call_ids(events: &[SourceEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            SourceEvent::Call(c) => Some(c.native_id.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn sessions_prompts_and_finished_calls_are_read() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("opencode.db");
    drop(fixture(&path));
    let src = OpencodeDb::new(&path);
    let events = visit(&src, &HashMap::new(), Some(Path::new("/work")));

    let sessions: Vec<(String, Option<String>)> = events
        .iter()
        .filter_map(|e| match e {
            SourceEvent::Session(s) => Some((s.native_id.clone(), s.parent.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        sessions,
        [
            ("ses_root".into(), None),
            ("ses_child".into(), Some("ses_root".into()))
        ],
        "root before its sub-agent; the out-of-scope session is skipped"
    );
    let prompts: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            SourceEvent::Prompt(p) => Some(p.native_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        prompts,
        ["msg_u1"],
        "synthetic and sub-agent turns are not prompts"
    );
    assert_eq!(
        call_ids(&events),
        ["call_r", "call_b", "call_p", "call_g"],
        "running calls wait"
    );

    let call = |id: &str| {
        events
            .iter()
            .find_map(|e| match e {
                SourceEvent::Call(c) if c.native_id == id => Some(c.clone()),
                _ => None,
            })
            .unwrap()
    };
    let read = call("call_r");
    assert_eq!(read.file_path.as_deref(), Some("/work/repo/src/parser.rs"));
    assert_eq!(read.line, Some(5));
    assert_eq!(read.result.as_ref().unwrap().output_bytes, 5);
    let bash = call("call_b");
    let result = bash.result.as_ref().unwrap();
    assert_eq!(result.exit_code, Some(101));
    assert!(result.excerpt.as_deref().unwrap().contains("E0433"));
    assert_eq!(call("call_p").extra_paths, ["src/parser.rs"]);
    assert_eq!(call("call_g").pattern.as_deref(), Some("fn parse_expr"));

    // The root's checkpoint resumes at the still-running call.
    let cps = checkpoints(&events);
    let root_key = key_hash("oc:ses_root");
    assert_eq!(cps[&root_key], "9000:1340");
}

#[test]
fn unchanged_sessions_are_skipped_and_updated_ones_resume() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("opencode.db");
    let conn = fixture(&path);
    let src = OpencodeDb::new(&path);
    let first = visit(&src, &HashMap::new(), Some(Path::new("/work")));
    let cps = checkpoints(&first);
    assert!(visit(&src, &cps, Some(Path::new("/work"))).is_empty());

    // The running call finishes; the session's time_updated moves.
    conn.execute_batch(
        "UPDATE part SET data = json_set(data, '$.state.status', 'completed', '$.state.metadata.exit', 0)
           WHERE id = 'p6';
         UPDATE session SET time_updated = 9500 WHERE id = 'ses_root';",
    )
    .unwrap();
    let second = visit(&src, &cps, Some(Path::new("/work")));
    assert_eq!(call_ids(&second), ["call_run"], "{second:#?}");
    assert_eq!(checkpoints(&second)[&key_hash("oc:ses_root")], "9500:1341");
}

#[test]
fn a_missing_database_is_not_created() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("absent.db");
    let src = OpencodeDb::new(&path);
    assert!(visit(&src, &HashMap::new(), None).is_empty());
    assert!(!path.exists());
}

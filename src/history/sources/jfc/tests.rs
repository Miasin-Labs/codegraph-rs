use super::*;

fn parse(log: &str) -> Vec<RawToolCall> {
    parse_log(log.as_bytes(), "ses_test").unwrap()
}

fn by_id<'a>(calls: &'a [RawToolCall], id: &str) -> &'a RawToolCall {
    calls
        .iter()
        .find(|c| c.native_id == id)
        .unwrap_or_else(|| panic!("no call {id} in {calls:#?}"))
}

/// One Bash call as a 2026-06 build logs it: announced four ways, executed
/// inside an `execute_tool{kind=Bash}` span whose every line repeats `kind=`.
const SPAN_DUPLICATED: &str = "\
2026-06-01T10:00:00.000001Z  INFO stream_response{provider=anthropic model=opus messages=3}: jfc::provider::anthropic_sse: content_block_start tool_use index=1 tool_name=Bash tool_use_id=toolu_A
2026-06-01T10:00:00.000002Z DEBUG stream_response{provider=anthropic model=opus messages=3}: jfc::stream: tool_done index=1 tool_name=Bash tool_use_id=toolu_A input_len=40
2026-06-01T10:00:00.000003Z  INFO jfc::ui::tool: StreamTool received tool_kind=Bash tool_id=toolu_A auto_mode=true needs_approval=false streaming_idx=0
2026-06-01T10:00:00.000004Z DEBUG jfc::scheduler: executing sequential tool tool_id=toolu_A kind=Bash
2026-06-01T10:00:00.000005Z  INFO execute_tool{active_team_name=None kind=Bash}: jfc::tools: bash: executing cmd=cd /repo && cargo test -p foo timeout_ms=120000 cwd=/repo
2026-06-01T10:00:00.000006Z DEBUG execute_tool{active_team_name=None kind=Bash}: jfc::tools: bash: completed exit_code=0 stdout_len=10 stderr_len=0
2026-06-01T10:00:00.000007Z DEBUG execute_tool{active_team_name=None kind=Bash}: jfc::context: invalidating cache entry path=/repo/x
2026-06-01T10:00:00.000008Z  INFO jfc::scheduler: tool completed tool_id=toolu_A kind=Bash outcome=Success output_len=10
2026-06-01T10:00:00.000009Z  INFO jfc::stream: tool_result received tool_id=toolu_A is_error=false output_len=10
2026-06-01T10:00:01.000001Z DEBUG stream_response{provider=bedrock model=opus messages=5}: jfc::stream: tool_done index=0 tool_name=read tool_use_id=tooluse_B input_len=30
2026-06-01T10:00:01.000002Z  INFO jfc::ui::tool: StreamTool received tool_kind=Read tool_id=tooluse_B auto_mode=true needs_approval=false streaming_idx=0
2026-06-01T10:00:01.000003Z DEBUG execute_tool{active_team_name=None kind=Read}:execute_tool_with_id{active_team_name=None tool_id=Some(\"tooluse_B\") kind=Read}: jfc::tools: read: starting file_path=/repo/src/my file.rs offset=1 limit=50
2026-06-01T10:00:01.000004Z DEBUG execute_tool{active_team_name=None kind=Read}:execute_tool_with_id{active_team_name=None tool_id=Some(\"tooluse_B\") kind=Read}: jfc::tools: read: success file_path=/repo/src/my file.rs line_count=50
";

#[test]
fn span_duplicated_lines_yield_one_call_per_tool_use_id() {
    let calls = parse(SPAN_DUPLICATED);
    assert_eq!(calls.len(), 2, "{calls:#?}");

    let bash = by_id(&calls, "toolu_A");
    assert_eq!(bash.tool, "Bash");
    assert_eq!(
        bash.command.as_deref(),
        Some("cd /repo && cargo test -p foo")
    );
    assert_eq!(bash.cwd.as_deref(), Some("/repo"));
    assert_eq!(bash.ts.as_deref(), Some("2026-06-01T10:00:00.000001Z"));
    assert_eq!(bash.session.as_deref(), Some("ses_test"));

    let read = by_id(&calls, "tooluse_B");
    assert_eq!(
        read.tool, "Read",
        "the parsed ToolKind beats the model's `read`"
    );
    assert_eq!(read.file_path.as_deref(), Some("/repo/src/my file.rs"));
}

#[test]
fn display_cmd_continues_across_lines_to_its_trailing_fields() {
    let log = "\
2026-06-02T09:00:00.000001Z  INFO execute_tool_inner{active_team_name=None runtime_tool_id=Some(\"toolu_C\") kind=Bash}: jfc::tools: bash: executing task_id=t1 cmd=cat > /tmp/x <<'EOF'
say timeout_ms=5 inside the heredoc
EOF
python3 /tmp/x timeout_ms=120000 track_for_abort=true cwd=/work dir output_path=/tmp/out
2026-06-02T09:00:00.000002Z  INFO execute_tool_inner{active_team_name=None runtime_tool_id=Some(\"toolu_D\") kind=Bash}: jfc::tools: bash: executing task_id=t2 cmd=echo  two  spaces
";
    let calls = parse(log);
    assert_eq!(calls.len(), 2);
    let c = by_id(&calls, "toolu_C");
    assert_eq!(c.tool, "Bash");
    assert_eq!(
        c.command.as_deref(),
        Some("cat > /tmp/x <<'EOF'\nsay timeout_ms=5 inside the heredoc\nEOF\npython3 /tmp/x")
    );
    assert_eq!(c.cwd.as_deref(), Some("/work dir"));
    // No trailing fields: the command runs to the end of the record.
    assert_eq!(
        by_id(&calls, "toolu_D").command.as_deref(),
        Some("echo  two  spaces")
    );
}

#[test]
fn quoted_cmd_from_older_builds_is_unescaped() {
    let log = "\
2026-05-05T08:00:00.000001Z DEBUG stream_response{provider=anthropic}: jfc::stream: tool_done index=0 tool_name=bash tool_use_id=toolu_Q input_len=9
2026-05-05T08:00:00.000002Z  INFO execute_tool{kind=Bash}: jfc::tools: bash: executing cmd=\"grep -n \\\"foo bar\\\" src\\nwc -l\" timeout_ms=1 cwd=/q
";
    let calls = parse(log);
    let q = by_id(&calls, "toolu_Q");
    assert_eq!(q.tool, "Bash");
    assert_eq!(q.command.as_deref(), Some("grep -n \"foo bar\" src\nwc -l"));
    assert_eq!(q.cwd.as_deref(), Some("/q"));
}

#[test]
fn details_without_ids_pair_with_waiting_calls_in_order() {
    let log = "\
2026-05-06T08:00:00.000001Z DEBUG s{}: jfc::stream: tool_done index=0 tool_name=Bash tool_use_id=toolu_1 input_len=1
2026-05-06T08:00:00.000002Z DEBUG s{}: jfc::stream: tool_done index=1 tool_name=Edit tool_use_id=toolu_2 input_len=1
2026-05-06T08:00:00.000003Z DEBUG s{}: jfc::stream: tool_done index=2 tool_name=Bash tool_use_id=toolu_3 input_len=1
2026-05-06T08:00:00.000004Z  INFO execute_tool{kind=Bash}: jfc::tools: bash: executing cmd=ls -la timeout_ms=1 cwd=/a
2026-05-06T08:00:00.000005Z  INFO execute_tool{kind=Edit}: jfc::tools: edit: starting file_path=/a/b.rs old_len=1 new_len=2 replace_all=false
2026-05-06T08:00:00.000006Z  INFO execute_tool{kind=Bash}: jfc::tools: bash: executing cmd=git status timeout_ms=1 cwd=/a
";
    let calls = parse(log);
    assert_eq!(calls.len(), 3);
    assert_eq!(by_id(&calls, "toolu_1").command.as_deref(), Some("ls -la"));
    assert_eq!(
        by_id(&calls, "toolu_2").file_path.as_deref(),
        Some("/a/b.rs")
    );
    assert_eq!(
        by_id(&calls, "toolu_3").command.as_deref(),
        Some("git status")
    );
}

#[test]
fn every_provider_announcement_form_registers_the_call() {
    let log = "\
2026-05-10T08:00:00.000001Z  INFO jfc::provider::openwebui: synthesize tool_done from accumulator index=0 tool_name=bash tool_use_id=call_ow1 args_len=20
2026-05-10T08:00:00.000002Z  INFO jfc::ui::tool: route=scheduler (streaming-tool-exec OFF, no approval needed) tool_kind=Grep tool_id=call_ui2 pending_total=1
2026-05-10T08:00:00.000003Z  INFO jfc::ui::approval: approved → dispatch tool_kind=Write tool_id=call_ap3
2026-05-10T08:00:00.000004Z  INFO execute_tool{active_team_name=None kind=Bash}: jfc::tools: bash: executing cmd=make test timeout_ms=1 cwd=/w
";
    let calls = parse(log);
    assert_eq!(calls.len(), 3, "{calls:#?}");
    let ow = by_id(&calls, "call_ow1");
    assert_eq!(ow.tool, "Bash");
    assert_eq!(
        ow.command.as_deref(),
        Some("make test"),
        "the detail pairs with the synthesized call"
    );
    assert_eq!(by_id(&calls, "call_ui2").tool, "Grep");
    assert_eq!(by_id(&calls, "call_ap3").tool, "Write");
}

#[test]
fn unannounced_details_get_stable_line_keyed_ids() {
    let log = "\
2026-05-07T08:00:00.000001Z  INFO jfc::tools: bash: executing cmd=make timeout_ms=1 cwd=/m
2026-05-07T08:00:00.000002Z  INFO jfc::tools: write: starting file_path=/m/out.txt content_len=3
";
    let first = parse(log);
    assert_eq!(first.len(), 2);
    assert_eq!(first[0].native_id, "ses_test#L1");
    assert_eq!(first[0].tool, "Bash");
    assert_eq!(first[1].native_id, "ses_test#L2");
    assert_eq!(first[1].tool, "Write");
    assert_eq!(parse(log), first, "re-parsing must yield the same ids");
}

#[test]
fn non_tool_kind_lines_are_not_calls() {
    let log = "\
2026-05-08T08:00:00.000001Z DEBUG jfc::watcher: fs event kind=Modify(Name(Both)) path=/x
2026-05-08T08:00:00.000002Z DEBUG jfc::stream: stream block kind=reasoning len=5
2026-05-08T08:00:00.000003Z DEBUG execute_tool{active_team_name=None kind=Read}:execute_tool_with_id{active_team_name=None tool_id=None kind=Read}: jfc::tools: read: success file_path=/x line_count=2
2026-05-08T08:00:00.000004Z DEBUG dispatch_tools_batched{tool_calls=[ToolCall { id: ToolId(\"x\"), kind: Bash, input: Bash { command: \"echo content_block_start tool_use tool_use_id=fake tool_name=Bash\" } }]}: jfc::stream: dispatching
2026-05-08T08:00:00.000005Z  INFO jfc::tracing: tracing initialized log_dir=/x
";
    assert_eq!(parse(log), Vec::<RawToolCall>::new());
}

#[test]
fn session_project_root_fills_calls_without_their_own_cwd() {
    let log = "\
2026-05-09T08:00:00.000001Z DEBUG s{}: jfc::stream: tool_done index=0 tool_name=Read tool_use_id=toolu_R input_len=1
2026-05-09T08:00:00.000002Z DEBUG dispatch_tools_batched{n=1}: jfc::agents: loading agents project_root=/home/u/repo
2026-05-09T08:00:00.000003Z DEBUG s{}: jfc::stream: tool_done index=1 tool_name=Bash tool_use_id=toolu_S input_len=1
2026-05-09T08:00:00.000004Z  INFO execute_tool{kind=Bash}: jfc::tools: bash: executing cmd=pwd timeout_ms=1 cwd=/home/u/other
";
    let calls = parse(log);
    // Announced before any project_root: backfilled with the first one seen.
    assert_eq!(
        by_id(&calls, "toolu_R").cwd.as_deref(),
        Some("/home/u/repo")
    );
    // A Bash detail's own cwd wins.
    assert_eq!(
        by_id(&calls, "toolu_S").cwd.as_deref(),
        Some("/home/u/other")
    );
}

#[test]
fn mcp_and_model_spelled_names_are_canonical() {
    assert_eq!(
        canonical_tool("Mcp(\"mcp__codegraph__codegraph_explore\")").as_deref(),
        Some("mcp__codegraph__codegraph_explore")
    );
    assert_eq!(
        canonical_tool("mcp__playwright__browser_snapshot").as_deref(),
        Some("mcp__playwright__browser_snapshot")
    );
    assert_eq!(canonical_tool("bash").as_deref(), Some("Bash"));
    assert_eq!(canonical_tool("taskupdate").as_deref(), Some("TaskUpdate"));
    assert_eq!(
        canonical_tool("graph_search").as_deref(),
        Some("GraphSearch")
    );
    assert_eq!(canonical_tool("set_goal").as_deref(), Some("SetGoal"));
    assert_eq!(canonical_tool("research").as_deref(), Some("Research"));
    assert_eq!(
        canonical_tool("GraphSearch").as_deref(),
        Some("GraphSearch")
    );
    assert_eq!(
        canonical_tool("Read}:execute_tool_with_id{active_team_name=None").as_deref(),
        Some("Read")
    );
    assert_eq!(canonical_tool("None"), None);
    assert_eq!(canonical_tool(""), None);
}

#[test]
fn field_boundaries_and_values() {
    let line = "x runtime_tool_id=Some(\"a\") tool_id=b kind=Mcp(\"m\") q=\"v \\\"w\\\"\" z";
    assert_eq!(field(line, "tool_id").as_deref(), Some("b"));
    assert_eq!(
        field(line, "runtime_tool_id").as_deref(),
        Some("Some(\"a\")")
    );
    assert_eq!(field(line, "q").as_deref(), Some("v \"w\""));
    assert_eq!(field(line, "missing"), None);
    assert_eq!(span_id("Some(\"toolu_x\")").as_deref(), Some("toolu_x"));
    assert_eq!(span_id("None"), None);
    assert_eq!(
        timestamp("2026-06-01T10:00:00.000001Z  INFO x"),
        Some("2026-06-01T10:00:00.000001Z")
    );
    assert_eq!(timestamp("python3 /tmp/x timeout_ms=1"), None);
}

#[test]
fn log_files_skip_symlinks_and_non_logs() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("ses_1.log");
    std::fs::write(&real, SPAN_DUPLICATED).unwrap();
    std::fs::write(dir.path().join("jfc.log.2026-05-05"), "").unwrap();
    std::fs::write(dir.path().join("notes.txt"), "").unwrap();
    std::fs::create_dir(dir.path().join("daemon")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, dir.path().join("latest.log")).unwrap();

    let src = JfcLogs::new(dir.path());
    let names: Vec<String> = src
        .log_files()
        .unwrap()
        .iter()
        .map(|p| session_name(p.as_path()))
        .collect();
    assert_eq!(names, ["jfc.log.2026-05-05", "ses_1"]);

    let mut seen = Vec::new();
    let stats = src
        .visit(&mut |call| {
            seen.push(call);
            Ok(())
        })
        .unwrap();
    assert_eq!(
        stats,
        SourceStats {
            inputs: 2,
            skipped: 0,
            calls: 2,
            deferred: 0,
        }
    );
    assert_eq!(seen.len(), 2);
}

#[test]
fn a_missing_log_dir_is_empty_not_an_error() {
    let stats = JfcLogs::new("/nonexistent/jfc/logs")
        .visit(&mut |_| Ok(()))
        .unwrap();
    assert_eq!(stats, SourceStats::default());
}

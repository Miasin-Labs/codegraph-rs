#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_codegraph")
}

#[test]
fn normal_init_and_index_complete() {
    // Given: a small source project without an index.
    let project = tempfile::tempdir().expect("create project");
    fs::write(project.path().join("main.rs"), "fn main() {}\n").expect("write source");

    // When: init and index run under command supervision.
    let init = run_cli(project.path(), &["init"]);
    let index = run_cli(project.path(), &["index", "--quiet"]);

    // Then: normal completion remains successful.
    assert!(init.status.success(), "{init:?}");
    assert!(index.status.success(), "{index:?}");
}

#[test]
fn cli_init_and_index_orphans_exit_after_parent_death() {
    if run_orphan_fixture_process() {
        return;
    }

    for label in ["init", "index"] {
        assert_orphan_exits(label);
    }
}

fn run_cli(cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .expect("run CLI")
}

fn run_orphan_fixture_process() -> bool {
    let Ok(role) = std::env::var("CODEGRAPH_CLI_ORPHAN_ROLE") else {
        return false;
    };
    let label = std::env::var("CODEGRAPH_CLI_ORPHAN_LABEL").expect("fixture label");
    match role.as_str() {
        "child" => {
            let _supervision = codegraph::mcp::proxy::start_command_supervision(&label);
            println!("CHILD_PID={}", std::process::id());
            std::io::stdout().flush().expect("flush child pid");
            std::thread::sleep(Duration::from_secs(30));
        }
        "wrapper" => run_wrapper(&label),
        other => panic!("unknown orphan fixture role: {other}"),
    }
    true
}

fn run_wrapper(label: &str) {
    let log =
        fs::File::create(std::env::var("CODEGRAPH_CLI_ORPHAN_LOG").expect("fixture log path"))
            .expect("create child stderr log");
    let mut child = Command::new(std::env::current_exe().expect("current test executable"))
        .arg("cli_init_and_index_orphans_exit_after_parent_death")
        .arg("--nocapture")
        .env("CODEGRAPH_CLI_ORPHAN_ROLE", "child")
        .env("CODEGRAPH_CLI_ORPHAN_LABEL", label)
        .env("CODEGRAPH_PPID_POLL_MS", "25")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(log))
        .spawn()
        .expect("spawn orphan fixture child");
    let pid = read_reported_pid(child.stdout.take().expect("child stdout"));
    println!("CHILD_PID={pid}");
    std::io::stdout().flush().expect("flush relayed child pid");
    std::thread::sleep(Duration::from_secs(30));
    let _ = child.kill();
    let _ = child.wait();
}

fn assert_orphan_exits(label: &str) {
    let log_dir = tempfile::tempdir().expect("create orphan fixture directory");
    let log_path = log_dir.path().join(format!("{label}.stderr.log"));
    let mut wrapper = Command::new(std::env::current_exe().expect("current test executable"))
        .arg("cli_init_and_index_orphans_exit_after_parent_death")
        .arg("--nocapture")
        .env("CODEGRAPH_CLI_ORPHAN_ROLE", "wrapper")
        .env("CODEGRAPH_CLI_ORPHAN_LABEL", label)
        .env("CODEGRAPH_CLI_ORPHAN_LOG", &log_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn orphan fixture wrapper");

    // Given: the command-supervision fixture is alive under a wrapper process.
    let child_pid = read_reported_pid(wrapper.stdout.take().expect("wrapper stdout"));
    assert!(codegraph::utils::is_process_alive(child_pid));

    // When: the wrapper dies without running cleanup.
    wrapper.kill().expect("kill wrapper");
    wrapper.wait().expect("reap wrapper");

    // Then: the orphan notices parent loss and exits nonzero on its own.
    let started = Instant::now();
    while codegraph::utils::is_process_alive(child_pid)
        && started.elapsed() < Duration::from_secs(5)
    {
        std::thread::sleep(Duration::from_millis(25));
    }
    if codegraph::utils::is_process_alive(child_pid) {
        let _ = Command::new("kill")
            .args(["-KILL", &child_pid.to_string()])
            .status();
        panic!("{label} orphan {child_pid} did not exit after parent death");
    }
    let stderr = fs::read_to_string(&log_path).expect("read child stderr log");
    assert!(
        stderr.contains(&format!("[CodeGraph {label}] Parent process exited")),
        "{stderr}"
    );
    assert!(stderr.contains("aborting"), "{stderr}");
}

fn read_reported_pid(stdout: impl std::io::Read) -> u32 {
    for line in BufReader::new(stdout).lines() {
        let line = line.expect("read fixture output");
        if let Some(pid) = line.strip_prefix("CHILD_PID=") {
            return pid.parse().expect("parse child pid");
        }
    }
    panic!("fixture exited before reporting child pid");
}

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use codegraph::CodeGraph;
use codegraph::mcp::daemon::{AcquireResult, try_acquire_daemon_lock};
use codegraph::mcp::daemon_paths::{get_daemon_pid_path, get_daemon_socket_path};
use serde_json::{Value, json};
use tempfile::TempDir;

const STARTUP_TIMEOUT: Duration = Duration::from_millis(150);
const EXIT_BOUND: Duration = Duration::from_secs(2);

struct ServerProcess {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl ServerProcess {
    fn spawn(root: &Path, direct: bool, startup_timeout: Duration) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_codegraph-mcp-server"));
        command
            .arg("--no-watch")
            .arg("--path")
            .arg(root)
            .current_dir(root)
            .env(
                "CODEGRAPH_HOME",
                concat!(env!("CARGO_TARGET_TMPDIR"), "/codegraph-home"),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_remove("CODEGRAPH_DAEMON_INTERNAL")
            .env_remove("CODEGRAPH_NO_DAEMON")
            .env("CODEGRAPH_MCP_DEBUG", "1")
            .env("CODEGRAPH_PPID_POLL_MS", "0")
            .env("CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS", "100")
            .env(
                "CODEGRAPH_STARTUP_HANDSHAKE_TIMEOUT_MS",
                startup_timeout.as_millis().to_string(),
            );
        if direct {
            command.env("CODEGRAPH_NO_DAEMON", "1");
        }
        let mut child = command.spawn().expect("spawn MCP server");
        let stdin = child.stdin.take();
        Self { child, stdin }
    }

    fn send_initialize(&mut self) {
        let line = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "startup-test", "version": "0" }
            }
        }))
        .expect("serialize initialize");
        let input = self.stdin.as_mut().expect("server stdin");
        input.write_all(line.as_bytes()).expect("write initialize");
        input.write_all(b"\n").expect("terminate initialize");
        input.flush().expect("flush initialize");
    }

    fn read_message(&mut self, timeout: Duration) -> Value {
        let stdout = self.child.stdout.take().expect("server stdout");
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let result = BufReader::new(stdout).read_line(&mut line).map(|_| line);
            let _ = sender.send(result);
        });
        let line = receiver
            .recv_timeout(timeout)
            .expect("server response before deadline")
            .expect("read server response");
        serde_json::from_str(&line).expect("server response JSON")
    }

    fn close_stdin(&mut self) {
        drop(self.stdin.take());
    }

    fn wait_for_exit(&mut self, timeout: Duration) -> std::process::ExitStatus {
        let started = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().expect("poll server") {
                return status;
            }
            assert!(
                started.elapsed() < timeout,
                "server did not exit within {timeout:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn stderr(&mut self) -> String {
        let mut stderr = String::new();
        self.child
            .stderr
            .take()
            .expect("server stderr")
            .read_to_string(&mut stderr)
            .expect("read server stderr");
        stderr
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn init_project(root: &Path) {
    let graph = CodeGraph::init_sync(root).expect("initialize CodeGraph project");
    graph.close();
}

fn wait_until_gone(path: &Path) {
    let started = Instant::now();
    while path.exists() {
        assert!(
            started.elapsed() < EXIT_BOUND,
            "{} still exists",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn direct_server_exits_when_no_mcp_traffic_arrives() {
    let project = TempDir::new().expect("temp project");
    let started = Instant::now();
    let mut server = ServerProcess::spawn(project.path(), true, STARTUP_TIMEOUT);

    let status = server.wait_for_exit(EXIT_BOUND);

    assert!(status.success());
    assert!(started.elapsed() < EXIT_BOUND);
    assert!(server.stderr().contains("No MCP traffic since startup"));
}

#[test]
fn proxy_exits_when_no_mcp_traffic_arrives() {
    let project = TempDir::new().expect("temp project");
    init_project(project.path());
    let started = Instant::now();
    let mut server = ServerProcess::spawn(project.path(), false, STARTUP_TIMEOUT);

    let status = server.wait_for_exit(EXIT_BOUND);

    assert!(status.success());
    assert!(started.elapsed() < EXIT_BOUND);
    assert!(server.stderr().contains("No MCP traffic since startup"));
    wait_until_gone(&get_daemon_pid_path(project.path()));
}

#[test]
fn direct_initialize_disarms_the_startup_timeout() {
    let project = TempDir::new().expect("temp project");
    let mut server = ServerProcess::spawn(project.path(), true, STARTUP_TIMEOUT);

    server.send_initialize();
    let response = server.read_message(EXIT_BOUND);
    std::thread::sleep(STARTUP_TIMEOUT * 3);

    assert_eq!(response["id"], 1);
    assert!(server.child.try_wait().expect("poll server").is_none());
    server.close_stdin();
    assert!(server.wait_for_exit(EXIT_BOUND).success());
}

#[test]
fn proxy_initialize_disarms_the_startup_timeout() {
    let project = TempDir::new().expect("temp project");
    init_project(project.path());
    let mut server = ServerProcess::spawn(project.path(), false, STARTUP_TIMEOUT);

    server.send_initialize();
    let response = server.read_message(EXIT_BOUND);
    std::thread::sleep(STARTUP_TIMEOUT * 3);

    assert_eq!(response["id"], 1);
    assert!(server.child.try_wait().expect("poll server").is_none());
    server.close_stdin();
    assert!(server.wait_for_exit(EXIT_BOUND).success());
    wait_until_gone(&get_daemon_pid_path(project.path()));
}

#[test]
fn withheld_daemon_hello_falls_back_within_the_shared_deadline() {
    let project = TempDir::new().expect("temp project");
    init_project(project.path());
    let canonical = std::fs::canonicalize(project.path()).expect("canonical project");
    assert!(matches!(
        try_acquire_daemon_lock(&canonical).expect("acquire fake daemon lock"),
        AcquireResult::Acquired { .. }
    ));
    let socket_path = get_daemon_socket_path(&canonical);
    let listener = UnixListener::bind(&socket_path).expect("bind withholding daemon");
    listener
        .set_nonblocking(true)
        .expect("set fake daemon nonblocking");
    let stop = Arc::new(AtomicBool::new(false));
    let stop_server = Arc::clone(&stop);
    let fake_daemon = std::thread::spawn(move || {
        while !stop_server.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((_connection, _)) => std::thread::sleep(Duration::from_millis(80)),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("accept proxy: {error}"),
            }
        }
    });
    let mut server = ServerProcess::spawn(project.path(), false, Duration::from_millis(300));
    server.send_initialize();
    let started = Instant::now();

    let response = server.read_message(Duration::from_secs(1));

    assert_eq!(response["id"], 1);
    assert!(started.elapsed() < Duration::from_secs(1));
    server.close_stdin();
    assert!(server.wait_for_exit(EXIT_BOUND).success());
    let stderr = server.stderr();
    assert!(stderr.contains("shared daemon unavailable"), "{stderr}");
    stop.store(true, Ordering::Release);
    fake_daemon.join().expect("fake daemon thread");
}

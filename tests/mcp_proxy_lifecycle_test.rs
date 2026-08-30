#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use codegraph::mcp::proxy::{HelloConnectResult, connect_with_hello, run_connected_proxy};
use codegraph::mcp::version::CODEGRAPH_PACKAGE_VERSION;

const ROLE_ENV: &str = "CODEGRAPH_PROXY_LIFECYCLE_ROLE";
const SOCKET_ENV: &str = "CODEGRAPH_PROXY_LIFECYCLE_SOCKET";
const EXIT_BOUND: Duration = Duration::from_secs(3);

#[test]
fn established_proxy_exits_promptly_when_daemon_dies() {
    if run_fixture_process() {
        return;
    }

    // Given: a byte proxy has completed its hello and owns an established session.
    let fixture = tempfile::tempdir().expect("create fixture directory");
    let socket = fixture.path().join("daemon.sock");
    let mut daemon = spawn_fixture("daemon", &socket);
    let mut daemon_stdout = BufReader::new(daemon.stdout.take().expect("daemon stdout"));
    wait_for_line(&mut daemon_stdout, "BOUND");
    let mut proxy = spawn_fixture("proxy", &socket);
    let proxy_stdin = proxy.stdin.take().expect("hold proxy stdin open");
    wait_for_line(&mut daemon_stdout, "SESSION");

    // When: the established daemon process is killed without a graceful disconnect.
    let started = Instant::now();
    daemon.kill().expect("kill fixture daemon");
    daemon.wait().expect("reap fixture daemon");

    // Then: the proxy closes promptly so the host can restart it; it never reconnects.
    let output = wait_for_exit(proxy, EXIT_BOUND);
    drop(proxy_stdin);
    assert!(output.status.success(), "{output:?}");
    assert!(started.elapsed() < EXIT_BOUND, "proxy exit was not prompt");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("serving this session in-process"),
        "{stderr}"
    );
}

fn run_fixture_process() -> bool {
    let Ok(role) = std::env::var(ROLE_ENV) else {
        return false;
    };
    let socket = std::env::var(SOCKET_ENV).expect("fixture socket path");
    match role.as_str() {
        "daemon" => run_daemon(Path::new(&socket)),
        "proxy" => run_proxy(Path::new(&socket)),
        other => panic!("unknown fixture role: {other}"),
    }
    true
}

fn run_daemon(socket: &Path) {
    let listener = UnixListener::bind(socket).expect("bind fixture daemon");
    println!("BOUND");
    std::io::stdout().flush().expect("flush bound marker");
    let (mut stream, _) = listener.accept().expect("accept proxy");
    writeln!(
        stream,
        "{{\"codegraph\":\"{CODEGRAPH_PACKAGE_VERSION}\",\"pid\":{},\"socketPath\":\"{}\",\"protocol\":1}}",
        std::process::id(),
        socket.display()
    )
    .expect("write daemon hello");
    println!("SESSION");
    std::io::stdout().flush().expect("flush session marker");
    std::thread::sleep(Duration::from_secs(30));
}

fn run_proxy(socket: &Path) {
    let HelloConnectResult::Connected(socket) =
        connect_with_hello(socket, CODEGRAPH_PACKAGE_VERSION)
    else {
        panic!("proxy failed to establish daemon session");
    };
    run_connected_proxy(socket);
}

fn spawn_fixture(role: &str, socket: &Path) -> Child {
    Command::new(std::env::current_exe().expect("current test executable"))
        .arg("established_proxy_exits_promptly_when_daemon_dies")
        .arg("--nocapture")
        .env(ROLE_ENV, role)
        .env(SOCKET_ENV, socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn lifecycle fixture")
}

fn wait_for_line(reader: &mut impl BufRead, expected: &str) {
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).expect("read fixture marker") == 0 {
            break;
        }
        let line = line.trim_end();
        if line == expected {
            return;
        }
    }
    panic!("fixture exited before {expected}");
}

fn wait_for_exit(mut child: Child, timeout: Duration) -> Output {
    let started = Instant::now();
    loop {
        if child.try_wait().expect("poll proxy").is_some() {
            return child.wait_with_output().expect("collect proxy output");
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let output = child.wait_with_output().expect("collect timed-out proxy");
            panic!("proxy did not exit after daemon loss: {output:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

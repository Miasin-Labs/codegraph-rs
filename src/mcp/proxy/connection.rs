use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::watchdog::start_ppid_watchdog;
use super::{CODEGRAPH_PACKAGE_VERSION, ProxyOutcome, ProxyResult};
use crate::mcp::daemon::{DaemonHello, MAX_HELLO_LINE_BYTES};

pub struct DaemonSocket {
    pub stream: UnixStream,
    pub tail: Vec<u8>,
}

pub enum HelloConnectResult {
    Connected(DaemonSocket),
    VersionMismatch,
    Unavailable,
}

fn read_hello_line(
    stream: &mut UnixStream,
    deadline: Instant,
) -> Result<(DaemonHello, Vec<u8>), String> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    let newline_at = loop {
        if let Some(index) = buffer.iter().position(|byte| *byte == b'\n') {
            break index;
        }
        if buffer.len() > MAX_HELLO_LINE_BYTES {
            return Err("daemon hello line exceeded size limit".to_string());
        }
        let now = Instant::now();
        if now >= deadline {
            return Err("timed out waiting for daemon hello".to_string());
        }
        let _ = stream.set_read_timeout(Some(deadline - now));
        match stream.read(&mut chunk) {
            Ok(0) => return Err("daemon closed connection before hello".to_string()),
            Ok(count) => {
                buffer.extend_from_slice(&chunk[..count]);
                if buffer.len() > MAX_HELLO_LINE_BYTES
                    && !buffer[..=MAX_HELLO_LINE_BYTES].contains(&b'\n')
                {
                    return Err("daemon hello line exceeded size limit".to_string());
                }
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                return Err("timed out waiting for daemon hello".to_string());
            }
            Err(error) => return Err(error.to_string()),
        }
    };
    let _ = stream.set_read_timeout(None);
    let line = &buffer[..newline_at];
    let tail = buffer[newline_at + 1..].to_vec();
    let parsed: Value =
        serde_json::from_slice(line).map_err(|error| format!("daemon hello not JSON: {error}"))?;
    let codegraph = parsed.get("codegraph").and_then(Value::as_str);
    let pid = parsed.get("pid").and_then(Value::as_f64);
    let (codegraph, pid) = match (codegraph, pid) {
        (Some(codegraph), Some(pid)) => (codegraph.to_string(), pid as u32),
        _ => return Err("daemon hello missing required fields".to_string()),
    };
    Ok((
        DaemonHello {
            codegraph,
            pid,
            socket_path: parsed
                .get("socketPath")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            protocol: parsed.get("protocol").and_then(Value::as_u64).unwrap_or(0) as u32,
        },
        tail,
    ))
}

pub fn connect_with_hello(socket_path: &Path, expected_version: &str) -> HelloConnectResult {
    connect_with_hello_until(
        socket_path,
        expected_version,
        Instant::now() + Duration::from_secs(3),
    )
}

pub(crate) fn connect_with_hello_until(
    socket_path: &Path,
    expected_version: &str,
    deadline: Instant,
) -> HelloConnectResult {
    if !socket_path.exists() {
        return HelloConnectResult::Unavailable;
    }
    let mut stream = match UnixStream::connect(socket_path) {
        Ok(stream) => stream,
        Err(_) => return HelloConnectResult::Unavailable,
    };
    let (hello, tail) = match read_hello_line(&mut stream, deadline) {
        Ok(hello) => hello,
        Err(_) => return HelloConnectResult::Unavailable,
    };
    if hello.codegraph != expected_version {
        eprintln!(
            "[CodeGraph MCP] Found a daemon on {} but version ({}) differs from ours ({}); serving this session in-process.",
            socket_path.display(),
            hello.codegraph,
            expected_version
        );
        return HelloConnectResult::VersionMismatch;
    }
    eprintln!(
        "[CodeGraph MCP] Attached to shared daemon on {} (pid {}, v{}).",
        socket_path.display(),
        hello.pid,
        hello.codegraph
    );
    HelloConnectResult::Connected(DaemonSocket { stream, tail })
}

pub fn run_connected_proxy(socket: DaemonSocket) -> ! {
    start_ppid_watchdog(&socket.stream);
    pipe_until_close(socket.stream, socket.tail);
    std::process::exit(0);
}

pub fn run_proxy(socket_path: &Path, expected_version: Option<&str>) -> ProxyResult {
    let expected_version = expected_version.unwrap_or(CODEGRAPH_PACKAGE_VERSION);
    if !socket_path.exists() {
        return ProxyResult {
            outcome: ProxyOutcome::FallbackNeeded,
            reason: Some("socket file missing".to_string()),
        };
    }
    match connect_with_hello(socket_path, expected_version) {
        HelloConnectResult::Connected(socket) => run_connected_proxy(socket),
        HelloConnectResult::VersionMismatch => ProxyResult {
            outcome: ProxyOutcome::FallbackNeeded,
            reason: Some("version mismatch".to_string()),
        },
        HelloConnectResult::Unavailable => ProxyResult {
            outcome: ProxyOutcome::FallbackNeeded,
            reason: Some("daemon unavailable".to_string()),
        },
    }
}

fn pipe_until_close(stream: UnixStream, tail: Vec<u8>) {
    let mut startup_timeout = crate::mcp::startup::arm_process_timeout(|| std::process::exit(0));
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    if !tail.is_empty() {
        let mut output = std::io::stdout().lock();
        let _ = output.write_all(&tail);
        let _ = output.flush();
    }

    {
        let mut socket = match stream.try_clone() {
            Ok(socket) => socket,
            Err(_) => return,
        };
        let done = done_tx.clone();
        std::thread::spawn(move || {
            let mut input = std::io::stdin().lock();
            let mut chunk = [0u8; 8192];
            loop {
                match input.read(&mut chunk) {
                    Ok(0) => {
                        let _ = socket.shutdown(std::net::Shutdown::Write);
                        return;
                    }
                    Ok(count) => {
                        if let Some(disarm) = startup_timeout.take() {
                            let _ = disarm.send(());
                        }
                        if socket.write_all(&chunk[..count]).is_err() {
                            let _ = done.send(());
                            return;
                        }
                    }
                    Err(_) => {
                        let _ = done.send(());
                        return;
                    }
                }
            }
        });
    }

    {
        let mut socket = stream;
        let done = done_tx;
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            loop {
                match socket.read(&mut chunk) {
                    Ok(0) => {
                        let _ = done.send(());
                        return;
                    }
                    Ok(count) => {
                        let mut output = std::io::stdout().lock();
                        if output.write_all(&chunk[..count]).is_err() {
                            let _ = done.send(());
                            return;
                        }
                        let _ = output.flush();
                    }
                    Err(error) => {
                        eprintln!("[CodeGraph MCP] daemon socket error: {error}");
                        let _ = done.send(());
                        return;
                    }
                }
            }
        });
    }

    let _ = done_rx.recv();
}

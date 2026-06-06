//! MCP daemon + proxy tests — issue #411 / #277 / #662.
//!
//! Ports the daemon-side cases of:
//! - `__tests__/mcp-daemon.test.ts` — lockfile arbitration (must-fix 1),
//!   versioned hello, refcounting, idle timeout, SIGTERM-with-clients (#662),
//!   stale-lock clearing, version-mismatch fallback.
//! - `__tests__/mcp-ppid-watchdog.test.ts` — #277 parent-death detection,
//!   exercised here against a real child process through the watchdog's
//!   host-pid channel (the TS four-tier process tree needs the full
//!   `serve --mcp` binary; see the note below).
//!
//! Test-split notes (for the orchestrator):
//! - `__tests__/mcp-debounce-env.test.ts` targets `parseDebounceEnv` in
//!   `src/mcp/engine.ts` → belongs to the MCP *server* agent (engine.rs).
//! - `__tests__/mcp-catchup-gate.test.ts` targets `ToolHandler.setCatchUpGate`
//!   in `src/mcp/tools.ts` → MCP server agent (tools.rs).
//! - The TS daemon suite spawns the real `codegraph serve --mcp` binary
//!   end-to-end (proxy ↔ detached daemon over a real socket). Those E2E specs
//!   need the CLI + MCPServer wiring; this file covers the same mechanics at
//!   the module boundary (real sockets, real lockfiles, real processes — no
//!   mocks). The remaining E2E re-validation belongs to the wiring task (see
//!   rust/notes/mcp-daemon.md).
//!
//! Everything socket-shaped is unix-gated: the Rust port has no named-pipe
//! listener on Windows (daemon mode is unix-only for now; Windows runs the
//! direct in-process path).

use std::path::Path;
use std::time::{Duration, Instant};

use codegraph::mcp::daemon::{
    AcquireResult,
    DEFAULT_IDLE_TIMEOUT_MS,
    clear_stale_daemon_lock,
    parse_idle_timeout_ms,
    try_acquire_daemon_lock,
};
use codegraph::mcp::daemon_paths::{
    DaemonLockInfo,
    HOST_PPID_ENV,
    decode_lock_info,
    encode_lock_info,
    get_daemon_pid_path,
    get_daemon_socket_path,
};
use codegraph::mcp::version::CODEGRAPH_PACKAGE_VERSION;

/// Poll `pred` every 25ms until it returns true or `timeout` elapses.
fn wait_for(pred: impl Fn() -> bool, timeout: Duration) -> bool {
    let start = Instant::now();
    loop {
        if pred() {
            return true;
        }
        if start.elapsed() > timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn temp_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(".codegraph")).expect(".codegraph dir");
    dir
}

// ---------------------------------------------------------------------------
// daemon-paths
// ---------------------------------------------------------------------------

#[test]
fn daemon_paths_are_deterministic_and_project_scoped() {
    let dir = temp_project();
    let sock1 = get_daemon_socket_path(dir.path());
    let sock2 = get_daemon_socket_path(dir.path());
    assert_eq!(
        sock1, sock2,
        "independent processes must converge on one socket path"
    );

    let pid_path = get_daemon_pid_path(dir.path());
    assert!(pid_path.ends_with(Path::new(".codegraph/daemon.pid")));

    let other = temp_project();
    assert_ne!(
        get_daemon_socket_path(dir.path()),
        get_daemon_socket_path(other.path()),
        "different roots must not share a socket"
    );
}

#[test]
fn lockfile_encoding_matches_ts_json_stringify() {
    let info = DaemonLockInfo {
        pid: 123,
        version: "1.2.3".to_string(),
        socket_path: "/tmp/x.sock".to_string(),
        started_at: 456,
    };
    // Byte parity with TS `JSON.stringify(info, null, 2) + '\n'` — the pidfile
    // is the rendezvous record shared between TS and Rust daemons.
    let expected = "{\n  \"pid\": 123,\n  \"version\": \"1.2.3\",\n  \"socketPath\": \"/tmp/x.sock\",\n  \"startedAt\": 456\n}\n";
    assert_eq!(encode_lock_info(&info), expected);
    assert_eq!(decode_lock_info(expected), Some(info));
}

// ---------------------------------------------------------------------------
// Lockfile arbitration (must-fix 1)
// ---------------------------------------------------------------------------

#[test]
fn acquire_writes_complete_record_atomically_then_reports_taken() {
    let dir = temp_project();
    let first = try_acquire_daemon_lock(dir.path()).expect("acquire");
    let pid_path = match first {
        AcquireResult::Acquired {
            ref pid_path,
            ref info,
        } => {
            assert_eq!(info.pid, std::process::id() as i64);
            assert_eq!(info.version, CODEGRAPH_PACKAGE_VERSION);
            assert_eq!(
                info.socket_path,
                get_daemon_socket_path(dir.path()).to_string_lossy()
            );
            assert!(info.started_at > 0);
            pid_path.clone()
        }
        AcquireResult::Taken { .. } => panic!("first acquire must win"),
    };

    // The pidfile is complete the instant it exists — no empty-file window.
    let on_disk = decode_lock_info(&std::fs::read_to_string(&pid_path).unwrap())
        .expect("pidfile must hold a complete record immediately");
    assert_eq!(on_disk.pid, std::process::id() as i64);

    // No temp files left behind.
    let leftovers: Vec<_> = std::fs::read_dir(pid_path.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "lock temp file must be unlinked");

    // Second candidate: lock is taken, existing record is readable.
    match try_acquire_daemon_lock(dir.path()).expect("second acquire") {
        AcquireResult::Acquired { .. } => panic!("second acquire must lose"),
        AcquireResult::Taken { existing, .. } => {
            let existing = existing.expect("atomic link means never a partial record");
            assert_eq!(existing.pid, std::process::id() as i64);
        }
    }
}

/// TS: "concurrent launchers converge on a single daemon (lockfile race —
/// must-fix 1)". Exactly one racer acquires; every loser reads a COMPLETE
/// record (the empty-pidfile window this guards against would decode to None
/// and let a loser unlink the winner's lock → two daemons).
#[test]
fn concurrent_acquires_converge_on_one_winner() {
    let dir = temp_project();
    let root = dir.path().to_path_buf();
    let mut handles = Vec::new();
    for _ in 0..8 {
        let root = root.clone();
        handles.push(std::thread::spawn(move || try_acquire_daemon_lock(&root)));
    }
    let mut acquired = 0;
    for handle in handles {
        match handle.join().unwrap().expect("acquire must not error") {
            AcquireResult::Acquired { .. } => acquired += 1,
            AcquireResult::Taken { existing, .. } => {
                let existing = existing.expect("losers must read a complete record");
                assert_eq!(existing.pid, std::process::id() as i64);
                assert_eq!(existing.version, CODEGRAPH_PACKAGE_VERSION);
            }
        }
    }
    assert_eq!(acquired, 1, "exactly one racer may hold the lock");
}

#[cfg(unix)]
fn spawn_dead_pid() -> i64 {
    // A real, definitely-exited process — sturdier than a magic number.
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg("exit 0")
        .spawn()
        .expect("spawn sh");
    let pid = child.id() as i64;
    child.wait().expect("reap child");
    pid
}

#[cfg(unix)]
#[test]
fn clear_stale_daemon_lock_clears_only_dead_holders() {
    let dir = temp_project();
    let pid_path = get_daemon_pid_path(dir.path());

    // Live holder (our own pid) → refuse to clear.
    let live = DaemonLockInfo {
        pid: std::process::id() as i64,
        version: "0.0.0-live".to_string(),
        socket_path: String::new(),
        started_at: 1,
    };
    std::fs::write(&pid_path, encode_lock_info(&live)).unwrap();
    assert!(!clear_stale_daemon_lock(&pid_path, Some(live.pid)));
    assert!(pid_path.exists(), "live daemon's lock must survive");

    // Dead holder → cleared (TS "clears a stale (dead-pid) lockfile").
    let dead_pid = spawn_dead_pid();
    let dead = DaemonLockInfo {
        pid: dead_pid,
        version: "0.0.0-fake".to_string(),
        socket_path: String::new(),
        started_at: 1,
    };
    std::fs::write(&pid_path, encode_lock_info(&dead)).unwrap();
    assert!(clear_stale_daemon_lock(&pid_path, Some(dead_pid)));
    assert!(!pid_path.exists());

    // Compare-and-delete: a different pid took over since the caller looked →
    // not ours to clear.
    std::fs::write(&pid_path, encode_lock_info(&dead)).unwrap();
    assert!(!clear_stale_daemon_lock(&pid_path, Some(dead_pid + 1)));
    assert!(pid_path.exists());

    // Already gone → success.
    std::fs::remove_file(&pid_path).unwrap();
    assert!(clear_stale_daemon_lock(&pid_path, Some(dead_pid)));
}

// ---------------------------------------------------------------------------
// Env parsing (CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS — parsed, not env-mutating)
// ---------------------------------------------------------------------------

#[test]
fn idle_timeout_env_parse_matches_ts() {
    assert_eq!(parse_idle_timeout_ms(None), DEFAULT_IDLE_TIMEOUT_MS);
    assert_eq!(parse_idle_timeout_ms(Some("")), DEFAULT_IDLE_TIMEOUT_MS);
    assert_eq!(parse_idle_timeout_ms(Some("nope")), DEFAULT_IDLE_TIMEOUT_MS);
    assert_eq!(parse_idle_timeout_ms(Some("-200")), DEFAULT_IDLE_TIMEOUT_MS);
    assert_eq!(parse_idle_timeout_ms(Some("15000")), 15_000);
    assert_eq!(parse_idle_timeout_ms(Some("0")), 0); // 0 = never idle-exit
}

// ---------------------------------------------------------------------------
// Daemon lifecycle over a real unix socket
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod unix_daemon_lifecycle {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::Arc;

    use codegraph::mcp::daemon::{Daemon, DaemonOptions, DaemonSessionFactory};
    use codegraph::mcp::proxy::{HelloConnectResult, ProxyOutcome, connect_with_hello, run_proxy};

    use super::*;

    /// Minimal stand-in for the MCPSession/MCPEngine seam: holds each
    /// connection open until the client side closes (mirrors a session
    /// blocking on its transport), echoes nothing.
    struct HoldOpenFactory;

    impl DaemonSessionFactory for HoldOpenFactory {
        fn serve_connection(&self, mut stream: UnixStream, _project_root: &Path) {
            let mut buf = [0u8; 256];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        }
    }

    fn start_daemon(root: &Path, idle_timeout_ms: u64) -> Daemon {
        match try_acquire_daemon_lock(root).expect("acquire") {
            AcquireResult::Acquired { .. } => {}
            AcquireResult::Taken { .. } => panic!("test root already locked"),
        }
        let daemon = Daemon::new(
            root,
            Arc::new(HoldOpenFactory),
            DaemonOptions {
                idle_timeout_ms: Some(idle_timeout_ms),
                // Installing process-wide SIGINT/SIGTERM handlers would hijack
                // the test harness — SIGTERM behavior is driven directly via
                // handle_sigterm() below.
                register_signal_handlers: false,
            },
        );
        daemon.start().expect("daemon start");
        daemon
    }

    fn read_hello_raw(stream: UnixStream) -> (String, UnixStream) {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).expect("hello line");
        (line, reader.into_inner())
    }

    /// TS: "two invocations share ONE detached daemon; both attach as proxies"
    /// — the module-level half: one daemon, versioned hello to every client,
    /// refcount tracks attach/detach.
    #[test]
    fn daemon_emits_versioned_hello_and_refcounts_clients() {
        let dir = temp_project();
        let daemon = start_daemon(dir.path(), 60_000);
        let sock = daemon.get_socket_path();
        assert!(sock.exists(), "socket file must exist at the computed path");

        // First client: hello arrives before any application bytes.
        let stream1 = UnixStream::connect(&sock).expect("connect 1");
        let (hello_line, stream1) = read_hello_raw(stream1);
        let hello: serde_json::Value = serde_json::from_str(&hello_line).expect("hello is JSON");
        assert_eq!(hello["codegraph"], CODEGRAPH_PACKAGE_VERSION);
        assert_eq!(hello["pid"], std::process::id());
        assert_eq!(hello["socketPath"], sock.to_string_lossy().as_ref());
        assert_eq!(hello["protocol"], 1);
        // Wire-shape parity with TS JSON.stringify key order.
        assert!(hello_line.starts_with("{\"codegraph\":\""));
        assert!(hello_line.trim_end().ends_with("\"protocol\":1}"));

        assert!(wait_for(
            || daemon.get_client_count() == 1,
            Duration::from_secs(5)
        ));

        // Second invocation attaches to the SAME daemon.
        let stream2 = UnixStream::connect(&sock).expect("connect 2");
        let (hello2, stream2) = read_hello_raw(stream2);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&hello2).unwrap()["pid"],
            std::process::id()
        );
        assert!(wait_for(
            || daemon.get_client_count() == 2,
            Duration::from_secs(5)
        ));

        // Disconnects decrement the refcount.
        drop(stream1);
        assert!(wait_for(
            || daemon.get_client_count() == 1,
            Duration::from_secs(5)
        ));
        drop(stream2);
        assert!(wait_for(
            || daemon.get_client_count() == 0,
            Duration::from_secs(5)
        ));

        daemon.stop("test");
        assert!(wait_for(|| daemon.is_stopped(), Duration::from_secs(5)));
        assert!(
            !get_daemon_pid_path(dir.path()).exists(),
            "stop cleans the lockfile"
        );
        assert!(!sock.exists(), "stop removes the socket file");
    }

    /// TS: "daemon idle-times-out after the last client disconnects" — and the
    /// idle timer is disarmed while a client is attached.
    #[test]
    fn daemon_idle_times_out_after_last_client_disconnects() {
        let dir = temp_project();
        let daemon = start_daemon(dir.path(), 300);
        let sock = daemon.get_socket_path();

        let stream = UnixStream::connect(&sock).expect("connect");
        let (_hello, stream) = read_hello_raw(stream);
        assert!(wait_for(
            || daemon.get_client_count() == 1,
            Duration::from_secs(5)
        ));

        // Well past the idle window with a client attached → still alive.
        std::thread::sleep(Duration::from_millis(700));
        assert!(
            !daemon.is_stopped(),
            "idle timer must be disarmed while clients are attached"
        );

        // Last client leaves → refcount hits 0 → idle timer fires → daemon
        // exits and cleans up its lockfile.
        drop(stream);
        assert!(wait_for(|| daemon.is_stopped(), Duration::from_secs(10)));
        daemon.wait(); // returns immediately once stopped
        assert!(!get_daemon_pid_path(dir.path()).exists());
        assert!(!sock.exists());
    }

    /// A daemon nobody ever connects to (launcher died) idle-exits too.
    #[test]
    fn daemon_with_no_clients_idle_times_out() {
        let dir = temp_project();
        let daemon = start_daemon(dir.path(), 200);
        assert!(wait_for(|| daemon.is_stopped(), Duration::from_secs(10)));
    }

    /// TS: "daemon ignores SIGTERM while clients are attached (#662)".
    #[test]
    fn daemon_ignores_sigterm_while_clients_attached() {
        let dir = temp_project();
        let daemon = start_daemon(dir.path(), 30_000);
        let sock = daemon.get_socket_path();

        let stream = UnixStream::connect(&sock).expect("connect");
        let (_hello, stream) = read_hello_raw(stream);
        assert!(wait_for(
            || daemon.get_client_count() == 1,
            Duration::from_secs(5)
        ));

        daemon.handle_sigterm();
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !daemon.is_stopped(),
            "SIGTERM with attached clients must be ignored"
        );

        // The client can still talk to the daemon afterwards (write succeeds).
        let mut writable = stream.try_clone().expect("clone");
        assert!(
            writable
                .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n")
                .is_ok()
        );

        // Idle → SIGTERM is honored.
        drop(writable);
        drop(stream);
        assert!(wait_for(
            || daemon.get_client_count() == 0,
            Duration::from_secs(5)
        ));
        daemon.handle_sigterm();
        assert!(wait_for(|| daemon.is_stopped(), Duration::from_secs(5)));
    }

    /// Stale socket file from a SIGKILL'd predecessor must not wedge bind.
    #[test]
    fn daemon_clears_stale_socket_file_on_start() {
        let dir = temp_project();
        let sock = get_daemon_socket_path(dir.path());
        std::fs::write(&sock, b"stale").unwrap();
        let daemon = start_daemon(dir.path(), 60_000);
        let stream = UnixStream::connect(&sock).expect("connect after stale clear");
        let (hello, _stream) = read_hello_raw(stream);
        assert!(hello.contains("\"codegraph\""));
        daemon.stop("test");
        assert!(wait_for(|| daemon.is_stopped(), Duration::from_secs(5)));
    }

    // -----------------------------------------------------------------------
    // Proxy ↔ daemon handshake
    // -----------------------------------------------------------------------

    #[test]
    fn connect_with_hello_attaches_on_matching_version() {
        let dir = temp_project();
        let daemon = start_daemon(dir.path(), 60_000);
        let sock = daemon.get_socket_path();

        match connect_with_hello(&sock, CODEGRAPH_PACKAGE_VERSION) {
            HelloConnectResult::Connected(ds) => {
                assert!(
                    ds.tail.is_empty(),
                    "nothing follows the hello before the first request"
                );
                assert!(wait_for(
                    || daemon.get_client_count() == 1,
                    Duration::from_secs(5)
                ));
                drop(ds);
            }
            _ => panic!("same-version daemon must attach"),
        }
        assert!(wait_for(
            || daemon.get_client_count() == 0,
            Duration::from_secs(5)
        ));
        daemon.stop("test");
        assert!(wait_for(|| daemon.is_stopped(), Duration::from_secs(5)));
    }

    /// Mini-server answering with a mismatched-version hello — the TS
    /// "proxy falls back to direct mode on a daemon version mismatch" planted
    /// daemon, at the module boundary.
    fn spawn_mini_server(hello_line: Vec<u8>) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("fake.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        std::thread::spawn(move || {
            if let Ok((mut conn, _)) = listener.accept() {
                let _ = conn.write_all(&hello_line);
                // Hold briefly so the client finishes reading before close.
                std::thread::sleep(Duration::from_millis(200));
            }
        });
        (dir, sock)
    }

    #[test]
    fn connect_with_hello_refuses_version_mismatch() {
        let hello =
            "{\"codegraph\":\"0.0.0-mismatch\",\"pid\":1,\"socketPath\":\"x\",\"protocol\":1}\n";
        let (_dir, sock) = spawn_mini_server(hello.as_bytes().to_vec());
        match connect_with_hello(&sock, CODEGRAPH_PACKAGE_VERSION) {
            HelloConnectResult::VersionMismatch => {}
            _ => panic!("mismatched daemon must be refused as VersionMismatch"),
        }
    }

    #[test]
    fn connect_with_hello_unavailable_when_no_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("absent.sock");
        assert!(matches!(
            connect_with_hello(&missing, CODEGRAPH_PACKAGE_VERSION),
            HelloConnectResult::Unavailable
        ));
    }

    #[test]
    fn connect_with_hello_rejects_malformed_hellos() {
        // Not JSON.
        let (_d1, s1) = spawn_mini_server(b"not json at all\n".to_vec());
        assert!(matches!(
            connect_with_hello(&s1, CODEGRAPH_PACKAGE_VERSION),
            HelloConnectResult::Unavailable
        ));

        // Missing required fields.
        let (_d2, s2) = spawn_mini_server(b"{\"pid\":1}\n".to_vec());
        assert!(matches!(
            connect_with_hello(&s2, CODEGRAPH_PACKAGE_VERSION),
            HelloConnectResult::Unavailable
        ));

        // Oversized hello line (bounded against a malicious peer).
        let mut oversized = vec![b'a'; 5000];
        oversized.push(b'\n');
        let (_d3, s3) = spawn_mini_server(oversized);
        assert!(matches!(
            connect_with_hello(&s3, CODEGRAPH_PACKAGE_VERSION),
            HelloConnectResult::Unavailable
        ));
    }

    /// Hello tail preservation: bytes the daemon sends after the hello newline
    /// in the same burst must surface in `DaemonSocket::tail` (TS
    /// `socket.unshift(tail)`).
    #[test]
    fn connect_with_hello_preserves_tail_bytes() {
        let hello = format!(
            "{{\"codegraph\":\"{CODEGRAPH_PACKAGE_VERSION}\",\"pid\":1,\"socketPath\":\"x\",\"protocol\":1}}\n{{\"jsonrpc\":\"2.0\"}}\n"
        );
        let (_dir, sock) = spawn_mini_server(hello.into_bytes());
        match connect_with_hello(&sock, CODEGRAPH_PACKAGE_VERSION) {
            HelloConnectResult::Connected(ds) => {
                assert_eq!(ds.tail, b"{\"jsonrpc\":\"2.0\"}\n".to_vec());
            }
            _ => panic!("hello with tail must still connect"),
        }
    }

    #[test]
    fn run_proxy_falls_back_when_socket_missing_or_version_mismatch() {
        // Missing socket file → fallback, never exits the process.
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("absent.sock");
        let res = run_proxy(&missing, None);
        assert_eq!(res.outcome, ProxyOutcome::FallbackNeeded);
        assert_eq!(res.reason.as_deref(), Some("socket file missing"));

        // Version mismatch → fallback with the TS reason string.
        let hello =
            b"{\"codegraph\":\"0.0.0-mismatch\",\"pid\":1,\"socketPath\":\"x\",\"protocol\":1}\n"
                .to_vec();
        let (_d, sock) = spawn_mini_server(hello);
        let res = run_proxy(&sock, None);
        assert_eq!(res.outcome, ProxyOutcome::FallbackNeeded);
        assert_eq!(res.reason.as_deref(), Some("version mismatch"));
    }
}

// ---------------------------------------------------------------------------
// PPID watchdog (#277) — real child process
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod ppid_watchdog {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use codegraph::mcp::proxy::spawn_ppid_watchdog_with;

    use super::*;

    #[test]
    fn host_ppid_env_name_is_stable() {
        // The #277 contract: the relauncher exports the host pid under this
        // exact name; daemon/proxy watchdogs read it. Renaming breaks
        // cross-version (and TS↔Rust) process trees.
        assert_eq!(HOST_PPID_ENV, "CODEGRAPH_HOST_PPID");
    }

    /// The watchdog's host-pid channel against a real process: alive while the
    /// host lives, fires shortly after the host is SIGKILL'd. (The TS
    /// four-tier `wrapper → {stdin-holder, codegraph}` tree exercises the same
    /// detection through the full binary; that E2E variant lands with the CLI
    /// wiring.)
    #[test]
    fn watchdog_fires_when_host_process_dies() {
        let mut host = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn sleep");
        let host_pid = host.id();

        let fired = Arc::new(AtomicBool::new(false));
        let fired2 = Arc::clone(&fired);
        let reason_holder = Arc::new(std::sync::Mutex::new(String::new()));
        let reason2 = Arc::clone(&reason_holder);
        // original_ppid = our real parent, which stays alive for the duration
        // of the test — only the host-pid channel can trigger.
        let original_ppid = (unsafe { libc::getppid() }) as u32;
        spawn_ppid_watchdog_with(50, original_ppid, Some(host_pid), move |reason| {
            *reason2.lock().unwrap() = reason.to_string();
            fired2.store(true, Ordering::SeqCst);
        });

        // Host alive → no false positive.
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            !fired.load(Ordering::SeqCst),
            "watchdog must not fire while the host lives"
        );

        // SIGKILL the host (no cleanup runs, like a real OOM kill) and reap it
        // so it can't linger as a kill(0)-alive zombie.
        host.kill().expect("kill host");
        host.wait().expect("reap host");

        assert!(
            wait_for(|| fired.load(Ordering::SeqCst), Duration::from_secs(5)),
            "watchdog must fire within a few polls of host death"
        );
        let reason = reason_holder.lock().unwrap().clone();
        assert!(
            reason.contains(&format!("host pid {host_pid} exited")),
            "shutdown must come from the parent-death path, got: {reason}"
        );
    }

    #[test]
    fn watchdog_disabled_when_poll_is_zero() {
        let fired = Arc::new(AtomicBool::new(false));
        let fired2 = Arc::clone(&fired);
        // Dead host + poll 0 → watchdog never installed (TS `pollMs <= 0`).
        spawn_ppid_watchdog_with(0, 1, Some(u32::MAX - 1), move |_| {
            fired2.store(true, Ordering::SeqCst);
        });
        std::thread::sleep(Duration::from_millis(200));
        assert!(!fired.load(Ordering::SeqCst));
    }
}

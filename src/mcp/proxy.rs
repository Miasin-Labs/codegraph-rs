//! MCP proxy mode — issue #411.
//!
//! The proxy is a near-transparent stdio↔socket pipe. Once it has verified
//! the daemon's hello line (same major.minor.patch as ours), it does no
//! protocol parsing of its own: every byte the MCP host writes to the proxy's
//! stdin goes straight to the daemon socket, and every byte the daemon emits
//! goes straight to the host's stdout. Server-initiated JSON-RPC requests
//! (e.g. `roots/list`) flow through the same pipe transparently.
//!
//! Lifecycle expectations:
//!   - The proxy exits when the host stream closes (host stdin closed → daemon
//!     socket end).
//!   - If the daemon socket closes first, local-handshake proxy mode reconnects
//!     to a fresh daemon instead of orphaning the existing MCP session (#662).
//!   - Closing the socket on the proxy side is what tells the daemon to
//!     decrement its connected-clients refcount.
//!   - On a parent-process death we can't detect via stdin close (e.g. SIGKILL
//!     of the MCP host), the proxy's PPID watchdog catches it — same logic
//!     the direct-mode server uses; see issue #277.
//!
//! Port note (decoupling): the TS file imports `SERVER_INFO` /
//! `negotiatedProtocolVersion` (session.ts), `SERVER_INSTRUCTIONS`
//! (server-instructions.ts), `getStaticTools` (tools.ts) and `MCPEngine`
//! (engine.ts). Those modules are owned by the MCP-server port; to keep this
//! file independently compilable they are injected through
//! [`LocalHandshakeDeps`] — the JSON-RPC *shapes* written to the client stay
//! in this file and are byte-identical to TS. See `rust/notes/mcp-daemon.md`
//! for the wiring contract.

use serde_json::Value;

use crate::mcp::version::CODEGRAPH_PACKAGE_VERSION;

/// Default poll cadence for the PPID watchdog (same as the direct server).
pub const DEFAULT_PPID_POLL_MS: u64 = 5000;
pub const DAEMON_RECONNECT_RETRY_MS: u64 = 100;
pub const DAEMON_RECONNECT_MAX_RETRIES: u32 = 120;
pub const DAEMON_PROXY_PENDING_MAX_LINES: usize = 1000;
pub const DAEMON_PROXY_PENDING_MAX_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyOutcome {
    /// Successfully attached to a same-version daemon and piped stdio. The
    /// proxy stays alive until either end closes. (Like TS, [`run_proxy`]
    /// exits the process at pipe close, so this variant is never observed by
    /// a caller — it exists for API parity.)
    Proxied,
    /// The daemon rejected us (version mismatch / unreachable socket) and the
    /// caller should run the server in direct mode.
    FallbackNeeded,
}

#[derive(Debug)]
pub struct ProxyResult {
    pub outcome: ProxyOutcome,
    pub reason: Option<String>,
}

/// Parse a `CODEGRAPH_PPID_POLL_MS`-style value. Mirrors the TS `parsePollMs`:
/// unset/empty/non-numeric/negative → default (5000); `0` is meaningful
/// (watchdog disabled); floats floored.
pub fn parse_poll_ms(raw: Option<&str>) -> u64 {
    let raw = match raw {
        None => return DEFAULT_PPID_POLL_MS,
        Some("") => return DEFAULT_PPID_POLL_MS,
        Some(r) => r,
    };
    let parsed: f64 = match raw.trim().parse() {
        Ok(v) => v,
        Err(_) => return DEFAULT_PPID_POLL_MS,
    };
    if !parsed.is_finite() || parsed < 0.0 {
        return DEFAULT_PPID_POLL_MS;
    }
    parsed.floor() as u64
}

/// Parse the host PID propagated across a re-exec
/// ([`crate::mcp::daemon_paths::HOST_PPID_ENV`]). Returns a positive integer
/// PID, or `None` when unset/invalid — the direct-launch path, where the
/// watchdog falls back to ppid divergence. PIDs of 0/1 are rejected
/// (0 = unknown, 1 = init, i.e. already orphaned), so the watchdog doesn't
/// latch onto init.
pub fn parse_host_ppid(raw: Option<&str>) -> Option<u32> {
    let raw = raw?;
    if raw.is_empty() {
        return None;
    }
    let parsed: f64 = raw.trim().parse().ok()?;
    // TS: Number.isInteger(parsed) && parsed > 1
    if !parsed.is_finite() || parsed.fract() != 0.0 || parsed <= 1.0 || parsed > u32::MAX as f64 {
        return None;
    }
    Some(parsed as u32)
}

fn is_process_alive_local(pid: u32) -> bool {
    crate::utils::is_process_alive(pid)
}

// ---------------------------------------------------------------------------
// Unix-only socket plumbing. On Windows the TS proxy speaks to a named pipe;
// Rust std has no named-pipe client, so the daemon/proxy path is unix-gated
// and Windows wiring runs in direct (in-process) mode.
// ---------------------------------------------------------------------------
#[cfg(unix)]
pub use unix_proxy::*;

#[cfg(unix)]
mod unix_proxy {
    use std::io::{BufRead, Read, Write};
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use super::*;
    use crate::mcp::daemon::{DaemonHello, MAX_HELLO_LINE_BYTES};
    use crate::mcp::daemon_paths::HOST_PPID_ENV;

    /// A connected daemon socket with its hello already consumed.
    pub struct DaemonSocket {
        pub stream: UnixStream,
        /// Bytes received past the hello line's terminating newline (the TS
        /// `socket.unshift(tail)`) — already-emitted daemon output that must be
        /// processed before anything newly read from the stream.
        pub tail: Vec<u8>,
    }

    /// Result of [`connect_with_hello`] — mirrors the TS
    /// `net.Socket | 'version-mismatch' | null` union.
    pub enum HelloConnectResult {
        Connected(DaemonSocket),
        /// A daemon IS up but it's the wrong version — definitive, not a
        /// "not yet". Don't poll; serve in-process.
        VersionMismatch,
        /// No daemon yet — caller should keep polling.
        Unavailable,
    }

    fn current_ppid() -> u32 {
        (unsafe { libc::getppid() }) as u32
    }

    /// Read one CRLF/LF-terminated JSON line from the socket, parse it as the
    /// daemon hello, and return it plus any bytes past the newline. Bounded to
    /// [`MAX_HELLO_LINE_BYTES`] so a malicious or broken peer can't OOM us.
    /// Times out at 3s — a healthy daemon sends hello immediately on accept.
    fn read_hello_line(stream: &mut UnixStream) -> Result<(DaemonHello, Vec<u8>), String> {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut buffer: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 1024];
        let newline_at = loop {
            if let Some(idx) = buffer.iter().position(|&b| b == b'\n') {
                break idx;
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
                Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    return Err("timed out waiting for daemon hello".to_string());
                }
                Err(e) => return Err(e.to_string()),
            }
        };
        let _ = stream.set_read_timeout(None);
        let line = &buffer[..newline_at];
        let tail = buffer[newline_at + 1..].to_vec();
        let parsed: Value =
            serde_json::from_slice(line).map_err(|e| format!("daemon hello not JSON: {e}"))?;
        let codegraph = parsed.get("codegraph").and_then(|v| v.as_str());
        let pid = parsed.get("pid").and_then(|v| v.as_f64());
        let (codegraph, pid) = match (codegraph, pid) {
            (Some(c), Some(p)) => (c.to_string(), p as u32),
            _ => return Err("daemon hello missing required fields".to_string()),
        };
        let hello = DaemonHello {
            codegraph,
            pid,
            socket_path: parsed
                .get("socketPath")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            protocol: parsed.get("protocol").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        };
        Ok((hello, tail))
    }

    /// Connect to a daemon at `socket_path` and verify its hello (exact version
    /// match). Returns the live socket (hello already consumed) or an
    /// unavailable / version-mismatch outcome. Unlike [`run_proxy`] it does NOT
    /// pipe — the caller owns the socket. Used by the local-handshake proxy's
    /// background connect.
    pub fn connect_with_hello(socket_path: &Path, expected_version: &str) -> HelloConnectResult {
        if !socket_path.exists() {
            return HelloConnectResult::Unavailable;
        }
        let mut stream = match UnixStream::connect(socket_path) {
            Ok(s) => s,
            Err(_) => return HelloConnectResult::Unavailable,
        };
        let (hello, tail) = match read_hello_line(&mut stream) {
            Ok(ok) => ok,
            Err(_) => return HelloConnectResult::Unavailable, // no daemon yet — keep polling
        };
        if hello.codegraph != expected_version {
            // A daemon IS up but it's the wrong version — definitive, not a
            // "not yet". Don't poll; the caller serves in-process so we never
            // run stale-vs-new.
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

    /// Attempt to connect to the daemon at `socket_path` and pipe stdio through
    /// it.
    ///
    /// Returns when the connection failed early enough that the caller can
    /// still fall back to direct mode. When the connection succeeds, the
    /// process **exits** (status 0) once either end of the pipe closes — same
    /// as the TS original, whose `'proxied'` outcome is likewise unobservable.
    ///
    /// Daemon and proxy MUST match versions exactly. Mismatch returns
    /// `FallbackNeeded` so the caller can transparently start its own server.
    /// (We accept the cost of two concurrent servers in this case as the price
    /// of never silently running a stale daemon against newer client code.)
    pub fn run_proxy(socket_path: &Path, expected_version: Option<&str>) -> ProxyResult {
        let expected_version = expected_version.unwrap_or(CODEGRAPH_PACKAGE_VERSION);
        // POSIX: refuse to connect to a stale socket file that points at no
        // listening process. The exists() pre-check is cheap; a real
        // ECONNREFUSED below catches the rare "exists but unbound" race.
        if !socket_path.exists() {
            return ProxyResult {
                outcome: ProxyOutcome::FallbackNeeded,
                reason: Some("socket file missing".to_string()),
            };
        }

        let mut stream = match UnixStream::connect(socket_path) {
            Ok(s) => s,
            Err(e) => {
                return ProxyResult {
                    outcome: ProxyOutcome::FallbackNeeded,
                    reason: Some(e.to_string()),
                };
            }
        };

        let (hello, tail) = match read_hello_line(&mut stream) {
            Ok(ok) => ok,
            Err(msg) => {
                return ProxyResult {
                    outcome: ProxyOutcome::FallbackNeeded,
                    reason: Some(msg),
                };
            }
        };

        if hello.codegraph != expected_version {
            eprintln!(
                "[CodeGraph MCP] Found a daemon on {} but version ({}) differs from ours ({}); falling back to direct mode.",
                socket_path.display(),
                hello.codegraph,
                expected_version
            );
            return ProxyResult {
                outcome: ProxyOutcome::FallbackNeeded,
                reason: Some("version mismatch".to_string()),
            };
        }

        eprintln!(
            "[CodeGraph MCP] Attached to shared daemon on {} (pid {}, v{}).",
            socket_path.display(),
            hello.pid,
            hello.codegraph
        );

        start_ppid_watchdog(&stream);
        pipe_until_close(stream, tail);
        // Host disconnected (or the daemon went away). The proxy's only job is
        // the pipe; exit now so we don't linger.
        std::process::exit(0);
    }

    /// Pipe stdin → socket and socket → stdout. Returns once either end closes
    /// so the process can exit. Stdin EOF half-closes the socket (FIN) rather
    /// than destroying it, mirroring the TS `socket.end()`.
    fn pipe_until_close(stream: UnixStream, tail: Vec<u8>) {
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();

        // Anything the daemon sent past its hello belongs to the host.
        if !tail.is_empty() {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(&tail);
            let _ = out.flush();
        }

        // stdin → socket
        {
            let mut sock_write = match stream.try_clone() {
                Ok(s) => s,
                Err(_) => return,
            };
            let done = done_tx.clone();
            std::thread::spawn(move || {
                let mut stdin = std::io::stdin().lock();
                let mut chunk = [0u8; 8192];
                loop {
                    match stdin.read(&mut chunk) {
                        Ok(0) => {
                            // stdin end → half-close the socket (TS socket.end()).
                            let _ = sock_write.shutdown(std::net::Shutdown::Write);
                            let _ = done.send(());
                            return;
                        }
                        Ok(n) => {
                            if sock_write.write_all(&chunk[..n]).is_err() {
                                // socket may have errored — close path catches it
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

        // socket → stdout
        {
            let mut sock_read = stream;
            let done = done_tx;
            std::thread::spawn(move || {
                let mut chunk = [0u8; 8192];
                loop {
                    match sock_read.read(&mut chunk) {
                        Ok(0) => {
                            let _ = done.send(());
                            return;
                        }
                        Ok(n) => {
                            let mut out = std::io::stdout().lock();
                            let _ = out.write_all(&chunk[..n]);
                            let _ = out.flush();
                        }
                        Err(e) => {
                            eprintln!("[CodeGraph MCP] daemon socket error: {e}");
                            let _ = done.send(());
                            return;
                        }
                    }
                }
            });
        }

        let _ = done_rx.recv();
    }

    // -----------------------------------------------------------------------
    // PPID watchdog (#277)
    // -----------------------------------------------------------------------

    /// Core watchdog loop — polls every `poll_ms`, fires `on_death(reason)`
    /// once when the original parent changed or the host pid died. Exposed
    /// (crate-internal seam) so the process-lifecycle tests can drive it with
    /// a real child process; production callers go through
    /// [`start_ppid_watchdog`] / [`start_ppid_watchdog_no_socket`].
    pub fn spawn_ppid_watchdog_with(
        poll_ms: u64,
        original_ppid: u32,
        host_ppid: Option<u32>,
        on_death: impl FnOnce(&str) + Send + 'static,
    ) {
        if poll_ms == 0 {
            return;
        }
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(poll_ms));
                let current = current_ppid();
                let ppid_changed = current != original_ppid;
                let host_gone = matches!(host_ppid, Some(h) if !is_process_alive_local(h));
                if ppid_changed || host_gone {
                    let reason = if ppid_changed {
                        format!("ppid {original_ppid} -> {current}")
                    } else {
                        format!("host pid {} exited", host_ppid.unwrap_or(0))
                    };
                    on_death(&reason);
                    return;
                }
            }
        });
    }

    /// PPID watchdog mirroring the one in the direct-mode server — kills the
    /// proxy if the MCP host (or its proxy of a host, see
    /// [`HOST_PPID_ENV`]) goes away without closing stdin. Issue #277
    /// documents why we can't rely on stdin EOF on Linux: the parent may be
    /// SIGKILL'd and reparenting doesn't close pipes.
    ///
    /// The proxy's "kill" is just a socket close + exit — no SQLite or
    /// watchers to clean up, so this is cheap.
    fn start_ppid_watchdog(stream: &UnixStream) {
        let poll_ms = parse_poll_ms(std::env::var("CODEGRAPH_PPID_POLL_MS").ok().as_deref());
        if poll_ms == 0 {
            return;
        }
        let host_ppid = parse_host_ppid(std::env::var(HOST_PPID_ENV).ok().as_deref());
        let sock = stream.try_clone().ok();
        spawn_ppid_watchdog_with(poll_ms, current_ppid(), host_ppid, move |reason| {
            eprintln!("[CodeGraph MCP] Parent process exited ({reason}); shutting down.");
            if let Some(s) = sock {
                let _ = s.shutdown(std::net::Shutdown::Both);
            }
            std::process::exit(0);
        });
    }

    /// PPID watchdog for the local-handshake proxy — same #277 logic as
    /// [`start_ppid_watchdog`] but with no socket to close (the caller's
    /// shutdown handles teardown).
    fn start_ppid_watchdog_no_socket(on_death: impl FnOnce() + Send + 'static) {
        let poll_ms = parse_poll_ms(std::env::var("CODEGRAPH_PPID_POLL_MS").ok().as_deref());
        if poll_ms == 0 {
            return;
        }
        let host_ppid = parse_host_ppid(std::env::var(HOST_PPID_ENV).ok().as_deref());
        spawn_ppid_watchdog_with(poll_ms, current_ppid(), host_ppid, move |_reason| {
            eprintln!("[CodeGraph MCP] Parent process exited; shutting down.");
            on_death();
        });
    }

    // -----------------------------------------------------------------------
    // Local-handshake proxy (the cold-start fix)
    // -----------------------------------------------------------------------

    /// In-process fallback tool executor — the seam over `MCPEngine` (owned by
    /// the MCP-server port). Created lazily, ONLY if the daemon never comes up,
    /// preserving the "a broken daemon never wedges a session" guarantee. The
    /// implementation is expected to lazily `ensureInitialized(root)` its
    /// engine and run `getToolHandler().execute(name, args)`.
    pub trait LocalToolExecutor: Send {
        /// Execute one tool call; `Ok` becomes the JSON-RPC `result`, `Err`
        /// becomes `{ code: -32603, message }`.
        fn execute(&mut self, name: &str, arguments: &Value) -> Result<Value, String>;
        /// Engine teardown on proxy shutdown (TS `engine?.stop()`).
        fn stop(&mut self) {}
    }

    /// Dependencies the local-handshake proxy needs, injected by the MCP
    /// server wiring (which owns the daemon-spawn machinery and the engine
    /// factory) — plus the static handshake data the TS file imported directly
    /// from `session.ts` / `tools.ts` / `server-instructions.ts`.
    pub struct LocalHandshakeDeps {
        /// Probe → spawn → retry → hello-verify; returns a connected daemon
        /// socket, or `None` when the daemon path is genuinely unavailable
        /// (→ in-process fallback).
        pub get_daemon_socket: Box<dyn FnMut() -> Option<DaemonSocket> + Send>,
        /// Lazily create an in-process engine-backed executor — used ONLY if
        /// the daemon never comes up.
        pub make_local_executor: Box<dyn FnMut() -> Box<dyn LocalToolExecutor> + Send>,
        /// Project root for the fallback engine's lazy init (captured by the
        /// wiring's `make_local_executor`; kept here for parity/logging).
        pub root: PathBuf,
        /// TS `negotiatedProtocolVersion(clientVersion)` from session.ts.
        pub negotiate_protocol_version: Box<dyn Fn(Option<&Value>) -> String + Send + Sync>,
        /// TS `SERVER_INFO` from session.ts (e.g. `{"name":"codegraph","version":...}`).
        pub server_info: Value,
        /// TS `SERVER_INSTRUCTIONS` from server-instructions.ts.
        pub server_instructions: String,
        /// TS `getStaticTools()` from tools.ts — the static tool list (JSON array).
        pub static_tools: Box<dyn Fn() -> Value + Send + Sync>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum DaemonStatus {
        Connecting,
        Ready,
        Failed,
    }

    struct HandshakeState {
        status: DaemonStatus,
        /// Write half of the attached daemon socket.
        daemon: Option<UnixStream>,
        /// Bumped on every (re)attach; a reader thread for a stale generation
        /// must not trigger reconnect (TS `daemonSocket !== socket` check).
        generation: u64,
        /// Suppress the daemon's reply to the forwarded initialize.
        client_init_id: Option<Value>,
        client_init_line: Option<String>,
        /// Client lines buffered until the daemon resolves.
        pending: Vec<String>,
        pending_bytes: usize,
        shutting_down: bool,
        reconnecting: bool,
        executor: Option<Box<dyn LocalToolExecutor>>,
    }

    struct ProxyShared {
        state: Mutex<HandshakeState>,
        get_daemon_socket: Mutex<Box<dyn FnMut() -> Option<DaemonSocket> + Send>>,
        make_local_executor: Mutex<Box<dyn FnMut() -> Box<dyn LocalToolExecutor> + Send>>,
        negotiate_protocol_version: Box<dyn Fn(Option<&Value>) -> String + Send + Sync>,
        server_info: Value,
        server_instructions: String,
        static_tools: Box<dyn Fn() -> Value + Send + Sync>,
    }

    /// Build a JSON-RPC envelope with the TS key order (`jsonrpc`, `id`, then
    /// the payload). Mirrors `JSON.stringify({jsonrpc:'2.0', id, ...})`
    /// semantics: an *absent* request id omits the key entirely (TS
    /// `undefined`), while an explicit `null` id is preserved.
    fn rpc_envelope(id: Option<&Value>, key: &str, payload: Value) -> Value {
        let mut map = serde_json::Map::new();
        map.insert("jsonrpc".to_string(), Value::String("2.0".to_string()));
        if let Some(id) = id {
            map.insert("id".to_string(), id.clone());
        }
        map.insert(key.to_string(), payload);
        Value::Object(map)
    }

    fn write_client_value(value: &Value) {
        let line = match serde_json::to_string(value) {
            Ok(l) => l,
            Err(_) => return,
        };
        write_client_raw(line.as_bytes());
    }

    fn write_client_raw(line: &[u8]) {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(line);
        let _ = out.write_all(b"\n");
        let _ = out.flush(); // host gone → ignored
    }

    fn write_daemon_line(stream: &mut UnixStream, line: &str) {
        // TS: `line.endsWith('\n') ? line : line + '\n'` — write errors are
        // ignored (the close path catches a dead socket).
        let _ = stream.write_all(line.as_bytes());
        if !line.ends_with('\n') {
            let _ = stream.write_all(b"\n");
        }
        let _ = stream.flush();
    }

    /// Daemon-unavailable fallback: serve a client message in-process.
    /// Called with the state lock held (mirrors the single-threaded TS
    /// execution: nothing else progresses during a local call).
    fn handle_locally(shared: &ProxyShared, st: &mut HandshakeState, line: &str) {
        let msg: Value = match serde_json::from_str(line) {
            Ok(m) => m,
            Err(_) => return,
        };
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|m| m.as_str());
        if method == Some("tools/call") && id.is_some() {
            if st.executor.is_none() {
                let mut make = shared.make_local_executor.lock().unwrap();
                st.executor = Some(make());
            }
            let params = msg.get("params");
            let name = params
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("");
            let arguments = params
                .and_then(|p| p.get("arguments"))
                .filter(|a| !a.is_null())
                .cloned()
                .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
            match st.executor.as_mut().unwrap().execute(name, &arguments) {
                Ok(result) => write_client_value(&rpc_envelope(id.as_ref(), "result", result)),
                Err(message) => write_client_value(&rpc_envelope(
                    id.as_ref(),
                    "error",
                    serde_json::json!({ "code": -32603, "message": message }),
                )),
            }
        } else if method == Some("ping") && id.is_some() {
            write_client_value(&rpc_envelope(
                id.as_ref(),
                "result",
                Value::Object(serde_json::Map::new()),
            ));
        }
        // initialize already answered locally; notifications (initialized) need no reply.
    }

    /// Pending-queue overflow → permanent in-process fallback.
    fn drain_pending_locally(shared: &ProxyShared, st: &mut HandshakeState, current: Option<&str>) {
        if st.status != DaemonStatus::Failed {
            eprintln!(
                "[CodeGraph MCP] Shared daemon pending queue exceeded; serving this session in-process (degraded)."
            );
        }
        st.status = DaemonStatus::Failed;
        if let Some(d) = st.daemon.take() {
            let _ = d.shutdown(std::net::Shutdown::Both);
        }
        let buffered = std::mem::take(&mut st.pending);
        st.pending_bytes = 0;
        for line in &buffered {
            handle_locally(shared, st, line);
        }
        if let Some(line) = current {
            handle_locally(shared, st, line);
        }
    }

    fn enqueue_pending(shared: &ProxyShared, st: &mut HandshakeState, line: &str) {
        let line_bytes = line.len() + 1;
        if st.pending.len() >= DAEMON_PROXY_PENDING_MAX_LINES
            || st.pending_bytes + line_bytes > DAEMON_PROXY_PENDING_MAX_BYTES
        {
            drain_pending_locally(shared, st, Some(line));
            return;
        }
        st.pending.push(line.to_string());
        st.pending_bytes += line_bytes;
    }

    fn route_to_daemon(shared: &Arc<ProxyShared>, line: &str) {
        let mut st = shared.state.lock().unwrap();
        if st.status == DaemonStatus::Ready && st.daemon.is_some() {
            if let Some(stream) = st.daemon.as_mut() {
                write_daemon_line(stream, line);
            }
        } else if st.status == DaemonStatus::Failed {
            handle_locally(shared, &mut st, line);
        } else {
            enqueue_pending(shared, &mut st, line);
        }
    }

    fn attach_daemon_socket(shared: &Arc<ProxyShared>, ds: DaemonSocket, replay_initialize: bool) {
        let write_half = match ds.stream.try_clone() {
            Ok(s) => s,
            Err(_) => {
                // Treat an unclonable socket as an immediate disconnect.
                on_daemon_disconnect_state(shared, None);
                return;
            }
        };
        let mut st = shared.state.lock().unwrap();
        st.generation += 1;
        let generation = st.generation;
        st.daemon = Some(write_half);
        st.status = DaemonStatus::Ready;

        {
            let shared2 = Arc::clone(shared);
            let stream = ds.stream;
            let tail = ds.tail;
            std::thread::spawn(move || daemon_reader(shared2, stream, tail, generation));
        }

        if replay_initialize {
            if let Some(init_line) = st.client_init_line.clone() {
                if let Some(d) = st.daemon.as_mut() {
                    write_daemon_line(d, &init_line); // reconnect path — errors ignored
                }
            }
        }
        // flushPendingToSocket
        let pending = std::mem::take(&mut st.pending);
        st.pending_bytes = 0;
        if let Some(d) = st.daemon.as_mut() {
            for line in &pending {
                write_daemon_line(d, line);
            }
        }
    }

    /// Reader side of an attached daemon socket: relay every line to the
    /// client, suppressing the daemon's reply to the forwarded `initialize`
    /// (the client already got the locally-answered one).
    fn daemon_reader(
        shared: Arc<ProxyShared>,
        mut stream: UnixStream,
        tail: Vec<u8>,
        generation: u64,
    ) {
        let mut buf = tail;
        let mut chunk = [0u8; 8192];
        loop {
            while let Some(idx) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=idx).take(idx).collect();
                let text = String::from_utf8_lossy(&line);
                if text.trim().is_empty() {
                    continue;
                }
                let suppress = {
                    let st = shared.state.lock().unwrap();
                    match (&st.client_init_id, serde_json::from_str::<Value>(&text)) {
                        (Some(init_id), Ok(m)) => {
                            m.get("id") == Some(init_id)
                                && (m.get("result").is_some() || m.get("error").is_some())
                        }
                        _ => false, // not JSON → relay
                    }
                };
                if suppress {
                    continue;
                }
                write_client_raw(&line);
            }
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        on_daemon_disconnect_state(&shared, Some(generation));
    }

    fn on_daemon_disconnect_state(shared: &Arc<ProxyShared>, generation: Option<u64>) {
        {
            let mut st = shared.state.lock().unwrap();
            if st.shutting_down {
                return;
            }
            if let Some(generation) = generation {
                if st.generation != generation {
                    return; // a newer socket already took over
                }
            }
            st.daemon = None;
            st.status = DaemonStatus::Connecting;
        }
        let shared2 = Arc::clone(shared);
        std::thread::spawn(move || reconnect_to_daemon(shared2));
    }

    fn reconnect_to_daemon(shared: Arc<ProxyShared>) {
        {
            let mut st = shared.state.lock().unwrap();
            if st.reconnecting || st.shutting_down {
                return;
            }
            st.reconnecting = true;
        }
        eprintln!("[CodeGraph MCP] Shared daemon connection lost; reconnecting.");
        for _attempt in 0..DAEMON_RECONNECT_MAX_RETRIES {
            if shared.state.lock().unwrap().shutting_down {
                break;
            }
            let socket = {
                let mut get = shared.get_daemon_socket.lock().unwrap();
                get()
            };
            if let Some(socket) = socket {
                if !shared.state.lock().unwrap().shutting_down {
                    eprintln!("[CodeGraph MCP] Reconnected to shared daemon.");
                    attach_daemon_socket(&shared, socket, true);
                    shared.state.lock().unwrap().reconnecting = false;
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(DAEMON_RECONNECT_RETRY_MS));
        }
        {
            let mut st = shared.state.lock().unwrap();
            if !st.shutting_down {
                st.status = DaemonStatus::Failed;
                eprintln!(
                    "[CodeGraph MCP] Shared daemon reconnect failed; serving this session in-process (degraded)."
                );
                let buffered = std::mem::take(&mut st.pending);
                st.pending_bytes = 0;
                for line in &buffered {
                    handle_locally(&shared, &mut st, line);
                }
            }
            st.reconnecting = false;
        }
    }

    fn shutdown(shared: &ProxyShared) {
        {
            let mut st = shared.state.lock().unwrap();
            if !st.shutting_down {
                st.shutting_down = true;
                if let Some(d) = st.daemon.take() {
                    let _ = d.shutdown(std::net::Shutdown::Both);
                }
                if let Some(mut executor) = st.executor.take() {
                    executor.stop();
                }
            }
        }
        std::process::exit(0);
    }

    /// One trimmed, non-empty line from the MCP host.
    fn process_client_line(shared: &Arc<ProxyShared>, line: &str) {
        let msg: Value = match serde_json::from_str(line) {
            Ok(m) => m,
            Err(_) => {
                route_to_daemon(shared, line);
                return;
            }
        };
        let method = msg.get("method").and_then(|m| m.as_str());
        match method {
            Some("initialize") => {
                {
                    let mut st = shared.state.lock().unwrap();
                    st.client_init_id = msg.get("id").cloned();
                    st.client_init_line = Some(line.to_string());
                }
                let client_version = msg.get("params").and_then(|p| p.get("protocolVersion"));
                let result = serde_json::json!({
                    "protocolVersion": (shared.negotiate_protocol_version)(client_version),
                    "capabilities": { "tools": {} },
                    "serverInfo": shared.server_info,
                    "instructions": shared.server_instructions,
                });
                write_client_value(&rpc_envelope(msg.get("id"), "result", result));
                // Prime the daemon so it resolves the project (its reply is
                // suppressed by the reader).
                route_to_daemon(shared, line);
            }
            Some("tools/list") => {
                let result = serde_json::json!({ "tools": (shared.static_tools)() });
                write_client_value(&rpc_envelope(msg.get("id"), "result", result));
            }
            Some("resources/list") => {
                // No resources exposed — answer the probe locally so it never
                // reaches the daemon as an unhandled method and logs `-32601`. (#621)
                let result = serde_json::json!({ "resources": [] });
                write_client_value(&rpc_envelope(msg.get("id"), "result", result));
            }
            Some("resources/templates/list") => {
                let result = serde_json::json!({ "resourceTemplates": [] });
                write_client_value(&rpc_envelope(msg.get("id"), "result", result));
            }
            Some("prompts/list") => {
                let result = serde_json::json!({ "prompts": [] });
                write_client_value(&rpc_envelope(msg.get("id"), "result", result));
            }
            _ => route_to_daemon(shared, line),
        }
    }

    /// Local-handshake proxy (the cold-start fix).
    ///
    /// Answers `initialize` + `tools/list` from STATIC constants the instant
    /// the client asks — tools register in ~process-startup time instead of
    /// waiting ~600ms for the daemon to spawn+bind, which is what produced the
    /// "No such tool available" race that made headless agents flail into
    /// grep/Read. Tool CALLS are forwarded to the shared daemon (connected in
    /// the background); the daemon's response to the forwarded `initialize` is
    /// suppressed (the client already got the local one). If the daemon never
    /// comes up (version mismatch / spawn fail), a lazily-created in-process
    /// engine serves the calls — so the handshake speedup never costs the old
    /// fall-back-to-direct robustness.
    ///
    /// Never returns: like the TS original it exits the process from its
    /// shutdown path (stdin EOF or PPID watchdog).
    pub fn run_local_handshake_proxy(deps: LocalHandshakeDeps) -> ! {
        let LocalHandshakeDeps {
            get_daemon_socket,
            make_local_executor,
            root: _root,
            negotiate_protocol_version,
            server_info,
            server_instructions,
            static_tools,
        } = deps;

        let shared = Arc::new(ProxyShared {
            state: Mutex::new(HandshakeState {
                status: DaemonStatus::Connecting,
                daemon: None,
                generation: 0,
                client_init_id: None,
                client_init_line: None,
                pending: Vec::new(),
                pending_bytes: 0,
                shutting_down: false,
                reconnecting: false,
                executor: None,
            }),
            get_daemon_socket: Mutex::new(get_daemon_socket),
            make_local_executor: Mutex::new(make_local_executor),
            negotiate_protocol_version,
            server_info,
            server_instructions,
            static_tools,
        });

        // ---- client (stdin) ----
        {
            let shared2 = Arc::clone(&shared);
            std::thread::spawn(move || {
                let stdin = std::io::stdin();
                let mut reader = stdin.lock();
                let mut raw: Vec<u8> = Vec::new();
                loop {
                    raw.clear();
                    match reader.read_until(b'\n', &mut raw) {
                        Ok(0) | Err(_) => break, // EOF / closed
                        Ok(_) => {
                            let text = String::from_utf8_lossy(&raw);
                            let line = text.trim();
                            if line.is_empty() {
                                continue;
                            }
                            process_client_line(&shared2, line);
                        }
                    }
                }
                shutdown(&shared2);
            });
        }

        {
            let shared2 = Arc::clone(&shared);
            start_ppid_watchdog_no_socket(move || shutdown(&shared2));
        }

        // ---- daemon connection (background relative to the handshake: the
        // stdin thread is already answering initialize/tools-list while this
        // connect — probe → spawn → retry — runs) ----
        let socket = {
            let mut get = shared.get_daemon_socket.lock().unwrap();
            get()
        };

        let shutting_down = shared.state.lock().unwrap().shutting_down;
        if let Some(socket) = socket {
            if !shutting_down {
                attach_daemon_socket(&shared, socket, false);
            }
        } else if !shutting_down {
            let mut st = shared.state.lock().unwrap();
            st.status = DaemonStatus::Failed;
            eprintln!(
                "[CodeGraph MCP] Shared daemon unavailable; serving this session in-process (degraded)."
            );
            let buffered = std::mem::take(&mut st.pending);
            st.pending_bytes = 0;
            for line in &buffered {
                handle_locally(&shared, &mut st, line);
            }
        }

        // stdin thread keeps the session alive; exit happens via shutdown().
        loop {
            std::thread::park();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_poll_ms_mirrors_ts() {
        assert_eq!(parse_poll_ms(None), DEFAULT_PPID_POLL_MS);
        assert_eq!(parse_poll_ms(Some("")), DEFAULT_PPID_POLL_MS);
        assert_eq!(parse_poll_ms(Some("abc")), DEFAULT_PPID_POLL_MS);
        assert_eq!(parse_poll_ms(Some("-5")), DEFAULT_PPID_POLL_MS);
        assert_eq!(parse_poll_ms(Some("0")), 0); // 0 = watchdog disabled
        assert_eq!(parse_poll_ms(Some("200")), 200);
        assert_eq!(parse_poll_ms(Some("250.9")), 250); // floored
    }

    #[test]
    fn parse_host_ppid_mirrors_ts() {
        assert_eq!(parse_host_ppid(None), None);
        assert_eq!(parse_host_ppid(Some("")), None);
        assert_eq!(parse_host_ppid(Some("abc")), None);
        assert_eq!(parse_host_ppid(Some("0")), None); // 0 = unknown
        assert_eq!(parse_host_ppid(Some("1")), None); // init — already orphaned
        assert_eq!(parse_host_ppid(Some("12.5")), None); // non-integer
        assert_eq!(parse_host_ppid(Some("2")), Some(2));
        assert_eq!(parse_host_ppid(Some("1e3")), Some(1000)); // Number() semantics
    }
}

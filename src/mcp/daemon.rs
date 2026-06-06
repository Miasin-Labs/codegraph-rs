//! Shared MCP daemon — issue #411.
//!
//! One detached `codegraph serve --mcp` daemon process per project root,
//! accepting N concurrent MCP clients over a Unix-domain socket (named pipe on
//! Windows — see the Windows note below). Each incoming connection gets its
//! own session; all sessions share a single engine, which means a single file
//! watcher (one inotify set), a single SQLite connection (one WAL writer), and
//! a single tree-sitter warm-up — paid once, amortized across every agent
//! talking to the project.
//!
//! Lifecycle (see also `server.rs` and `proxy.rs`):
//!   - The daemon is spawned **detached** (its own session/process group, stdio
//!     decoupled) by the first launcher that finds no daemon running. It is NOT
//!     a child of any MCP host, so closing one terminal / Ctrl-C'ing one session
//!     can't take it down and sever the others. That's why this process has no
//!     PPID watchdog: it deliberately outlives every individual client.
//!   - Every MCP host talks to the daemon through a thin `proxy` process (the
//!     thing the host actually spawned). The proxy keeps the #277 PPID watchdog,
//!     so a SIGKILL'd host still reaps its proxy promptly; the proxy's socket
//!     close then decrements the daemon's refcount.
//!   - When the last client disconnects the daemon lingers for
//!     `CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS` (default 300s) so back-to-back agent
//!     runs in the same project don't repay startup, then exits cleanly. This is
//!     what keeps a single-agent session from leaking a daemon forever (#277).
//!
//! What this file owns:
//!   - Listening on the daemon socket and spawning per-connection sessions.
//!   - The handshake "hello" line that lets a proxy verify it found a
//!     same-version daemon before piping any JSON-RPC through it.
//!   - The lockfile (`.codegraph/daemon.pid`) competing daemons arbitrate
//!     against — atomic exclusive create with the full record written in the
//!     same breath (no empty-file window) + cleanup on exit.
//!   - Reference counting + idle timeout.
//!   - Graceful shutdown on SIGTERM/SIGINT and idle exit.
//!
//! What this file does NOT own:
//!   - The proxy side (`proxy.rs`).
//!   - The decision of *whether* to run as daemon at all — that's `MCPServer`.
//!   - The MCP protocol state machine — that's `session.rs`. The per-connection
//!     session and shared engine are injected via [`DaemonSessionFactory`] so
//!     this file has no compile-time dependency on them.
//!
//! Windows port note: the TS daemon listens on a named pipe
//! (`\\.\pipe\codegraph-<hash>`) via Node's `net` module. Rust's std has no
//! named-pipe listener, so the socket-facing surface here is `#[cfg(unix)]`;
//! on Windows the wiring must run in direct (in-process) mode, exactly as if
//! `CODEGRAPH_NO_DAEMON=1` were set. The lockfile helpers and path computation
//! remain cross-platform.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::mcp::daemon_paths::{
    DaemonLockInfo,
    decode_lock_info,
    encode_lock_info,
    get_daemon_pid_path,
    get_daemon_socket_path,
};
use crate::mcp::version::CODEGRAPH_PACKAGE_VERSION;
use crate::utils::is_process_alive;

/// Default idle linger after the last client disconnects.
pub const DEFAULT_IDLE_TIMEOUT_MS: u64 = 300_000;

/// Bytes/parse-window for an oversized hello line — bounded against a malicious peer.
pub const MAX_HELLO_LINE_BYTES: usize = 4096;

/// Wire format for the one-shot hello line the daemon emits on every new
/// connection. Versioned with the package's own semver so a 0.9.x proxy never
/// pipes through a 0.10.x daemon (or vice-versa) — the proxy falls back to
/// direct mode on mismatch rather than risk subtle wire incompatibilities.
///
/// Serialized as one JSON line, camelCase keys in this exact order — byte
/// parity with `JSON.stringify` of the TS object literal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonHello {
    /// Package version (must match the proxy's own version).
    pub codegraph: String,
    /// Daemon pid (informational; for `ps` debugging).
    pub pid: u32,
    /// Echoed back so the proxy can log it.
    #[serde(default)]
    pub socket_path: String,
    /// Bump if the hello shape changes.
    #[serde(default)]
    pub protocol: u32,
}

/// Result of a successful [`Daemon::start`].
#[derive(Debug, Clone)]
pub struct DaemonStartResult {
    /// Always-non-null for a successfully-started daemon.
    pub socket_path: PathBuf,
    /// Lockfile contents as written.
    pub lock: DaemonLockInfo,
}

/// Result of [`try_acquire_daemon_lock`]. Either we got the lockfile (caller
/// becomes the daemon), or it already existed (caller should connect to the
/// existing daemon as a proxy, or — if the holder is dead — clear it and retry).
#[derive(Debug)]
pub enum AcquireResult {
    Acquired {
        pid_path: PathBuf,
        info: DaemonLockInfo,
    },
    Taken {
        existing: Option<DaemonLockInfo>,
        pid_path: PathBuf,
    },
}

/// Disambiguator appended to the lock temp-file name so concurrent acquires
/// inside ONE process (possible in Rust's threaded world, impossible in
/// single-threaded Node) never collide on the temp path. The TS name is
/// `<pidPath>.<pid>.tmp`; Rust adds a process-wide counter:
/// `<pidPath>.<pid>.<n>.tmp`. The temp file is unlinked before returning, so
/// nothing observable changes.
static LOCK_TMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Atomically create the daemon pidfile with its full record already in place.
/// Returns either an `Acquired` result (the caller is the daemon-elect and may
/// construct a [`Daemon`]) or a `Taken` result.
///
/// must-fix 1 (issue #411 review): the lockfile must appear in ONE atomic step,
/// already complete — never empty, even momentarily. The first attempt at this
/// (`O_EXCL` create then a separate write) left a microsecond window where
/// the file existed but was empty; under concurrent daemon startup a third
/// candidate could read that empty file, decode it as `None`, and unlink the
/// winner's lock → two daemons (two watchers, two writers).
///
/// The fix writes the complete record to a private temp file, then hard-links it
/// into place: `link()` is atomic AND exclusive (EEXIST if the target exists), so
/// the pidfile becomes visible in one step already containing a full record.
/// Whoever links first wins; everyone else gets EEXIST and reads a complete file.
/// There is no empty-file window at all.
pub fn try_acquire_daemon_lock(project_root: &Path) -> crate::error::Result<AcquireResult> {
    let pid_path = get_daemon_pid_path(project_root);
    // Make sure the .codegraph/ directory exists — the daemon may be the first
    // thing to touch it on a fresh-clone-but-already-initialized checkout.
    if let Some(parent) = pid_path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            crate::error::CodeGraphError::other(format!(
                "failed to create daemon lock directory: {e}"
            ))
        })?;
    }

    let info = DaemonLockInfo {
        pid: std::process::id() as i64,
        version: CODEGRAPH_PACKAGE_VERSION.to_string(),
        socket_path: get_daemon_socket_path(project_root)
            .to_string_lossy()
            .to_string(),
        started_at: now_ms(),
    };

    // Temp name is pid-scoped (+ in-process counter, see LOCK_TMP_COUNTER) so
    // racing candidates never collide on it.
    let nonce = LOCK_TMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let tmp = PathBuf::from(format!(
        "{}.{}.{}.tmp",
        pid_path.to_string_lossy(),
        std::process::id(),
        nonce
    ));

    let write_result = write_private_file(&tmp, encode_lock_info(&info).as_bytes());
    if let Err(e) = write_result {
        let _ = fs::remove_file(&tmp);
        return Err(crate::error::CodeGraphError::other(format!(
            "failed to write daemon lock temp file: {e}"
        )));
    }

    let link_result = fs::hard_link(&tmp, &pid_path); // atomic + exclusive
    let _ = fs::remove_file(&tmp); // temp already served its purpose either way

    let acquired = match link_result {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(e) => {
            return Err(crate::error::CodeGraphError::other(format!(
                "failed to link daemon lock file: {e}"
            )));
        }
    };

    if acquired {
        return Ok(AcquireResult::Acquired { pid_path, info });
    }

    // Taken. Because the pidfile was link'd atomically it always holds a complete
    // record — `existing` is None only for a genuinely corrupt leftover, never a
    // mid-write race.
    let existing = fs::read_to_string(&pid_path)
        .ok()
        .and_then(|raw| decode_lock_info(&raw));
    Ok(AcquireResult::Taken { existing, pid_path })
}

/// Remove a stale pidfile, but only if it still names a dead process. Re-reads
/// the file immediately before unlinking so we never delete a lock that a live
/// daemon (re)acquired in the meantime.
///
/// must-fix 1 (issue #411 review): the original unconditionally unlinked,
/// which let a racing candidate delete a healthy daemon's lock. Passing
/// `expected_dead_pid` (the pid the caller believed was dead) makes the clear a
/// compare-and-delete: bail if the file now holds a different pid, or any live
/// pid. Returns true when the stale lock is gone (or was already gone).
pub fn clear_stale_daemon_lock(pid_path: &Path, expected_dead_pid: Option<i64>) -> bool {
    let raw = match fs::read_to_string(pid_path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return true, // already gone
        Err(_) => return false,
    };
    if let Some(info) = decode_lock_info(&raw) {
        // A different pid took over since we read it — not ours to clear.
        if let Some(expected) = expected_dead_pid {
            if info.pid != expected {
                return false;
            }
        }
        // Holder is actually alive — never clear a live daemon's lock.
        if info.pid > 0 && is_process_alive(info.pid as u32) {
            return false;
        }
    }
    match fs::remove_file(pid_path) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false,
    }
}

/// Parse a `CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS`-style value. Unset/empty,
/// non-numeric, or negative values fall back to [`DEFAULT_IDLE_TIMEOUT_MS`].
/// `0` is meaningful: never idle-exit. Mirrors the TS `resolveIdleTimeoutMs`
/// (`Number(raw)` semantics: whitespace tolerated, scientific notation OK,
/// floats floored).
pub fn parse_idle_timeout_ms(raw: Option<&str>) -> u64 {
    let raw = match raw {
        None => return DEFAULT_IDLE_TIMEOUT_MS,
        Some("") => return DEFAULT_IDLE_TIMEOUT_MS,
        Some(r) => r,
    };
    let parsed: f64 = match raw.trim().parse() {
        Ok(v) => v,
        Err(_) => return DEFAULT_IDLE_TIMEOUT_MS,
    };
    if !parsed.is_finite() || parsed < 0.0 {
        return DEFAULT_IDLE_TIMEOUT_MS;
    }
    parsed.floor() as u64
}

/// Resolve the idle timeout from the `CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS` env var.
pub fn resolve_idle_timeout_ms() -> u64 {
    parse_idle_timeout_ms(
        std::env::var("CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS")
            .ok()
            .as_deref(),
    )
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(unix)]
fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    fs::write(path, bytes)
}

// ---------------------------------------------------------------------------
// Unix-only daemon (socket listener, refcount, idle timeout, signals).
// ---------------------------------------------------------------------------
#[cfg(unix)]
pub use unix_daemon::{Daemon, DaemonOptions, DaemonSessionFactory};

#[cfg(unix)]
mod unix_daemon {
    use std::collections::HashMap;
    use std::io::Write;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    use super::*;

    /// Per-connection session + shared engine seam.
    ///
    /// The TS daemon constructs `new MCPSession(new SocketTransport(socket),
    /// this.engine, { explicitProjectPath })` per connection, backgrounds
    /// `engine.ensureInitialized(projectRoot)` at start, and calls
    /// `engine.stop()` at shutdown. Those live in `session.rs` / `engine.rs`;
    /// the daemon stays decoupled by taking this factory, which the MCP server
    /// wiring implements over the real session/engine.
    pub trait DaemonSessionFactory: Send + Sync + 'static {
        /// Backgrounded engine warm-up — called once on a background thread when
        /// the daemon starts (TS: `void this.engine.ensureInitialized(root)`;
        /// deliberately backgrounded — see #172: the first session to land waits
        /// on `ensureInitialized` either way).
        fn warm_up(&self, _project_root: &Path) {}

        /// Serve one accepted client connection until the client disconnects.
        /// The daemon has already written the hello line. Runs on a dedicated
        /// thread; returning drops the client (decrements the refcount).
        fn serve_connection(&self, stream: UnixStream, project_root: &Path);

        /// Stop the shared engine (TS: `this.engine.stop()`) — called once
        /// during daemon shutdown.
        fn stop_engine(&self) {}
    }

    /// Construction options. `idle_timeout_ms: None` resolves from the
    /// `CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS` env var (default 300s).
    /// `register_signal_handlers` exists for tests — installing a process-wide
    /// SIGINT/SIGTERM handler inside a test harness would hijack the harness's
    /// own signal disposition. Production wiring keeps the default `true`.
    pub struct DaemonOptions {
        pub idle_timeout_ms: Option<u64>,
        pub register_signal_handlers: bool,
    }

    impl Default for DaemonOptions {
        fn default() -> Self {
            DaemonOptions {
                idle_timeout_ms: None,
                register_signal_handlers: true,
            }
        }
    }

    static SIGINT_FLAG: AtomicBool = AtomicBool::new(false);
    static SIGTERM_FLAG: AtomicBool = AtomicBool::new(false);

    extern "C" fn daemon_signal_handler(sig: libc::c_int) {
        // Async-signal-safe: just set a flag; the accept loop polls it.
        if sig == libc::SIGINT {
            SIGINT_FLAG.store(true, Ordering::SeqCst);
        } else if sig == libc::SIGTERM {
            SIGTERM_FLAG.store(true, Ordering::SeqCst);
        }
    }

    struct DaemonState {
        /// Connected clients: id → stream clone used to force-close on stop.
        clients: HashMap<u64, UnixStream>,
        idle_deadline: Option<Instant>,
        stopping: bool,
        stopped: bool,
    }

    struct DaemonInner {
        project_root: PathBuf,
        socket_path: PathBuf,
        pid_path: PathBuf,
        idle_timeout_ms: u64,
        signals_registered: bool,
        factory: Arc<dyn DaemonSessionFactory>,
        state: Mutex<DaemonState>,
        cv: Condvar,
        next_client_id: AtomicU64,
    }

    /// Run as the shared daemon for `project_root`. [`Daemon::start`] returns
    /// once the socket is listening. The Daemon owns the socket, the engine
    /// (via the factory), and the lockfile until [`Daemon::stop`] is called or
    /// it stops on idle/signal.
    ///
    /// Race-safe: callers must first call [`try_acquire_daemon_lock`] and only
    /// construct a Daemon if they got the lock (`AcquireResult::Acquired`). The
    /// atomic exclusive create inside the acquire helper — which also writes
    /// the full record before returning — is the only synchronization between
    /// competing daemons.
    ///
    /// Port note: TS `stop()` ends with `process.exit(0)`. The Rust port
    /// instead marks the daemon stopped and wakes [`Daemon::wait`]; the binary
    /// wiring exits the process after `wait()` returns. Observable behavior is
    /// identical for the daemon process; in-process tests stay alive.
    pub struct Daemon {
        inner: Arc<DaemonInner>,
    }

    impl Daemon {
        pub fn new(
            project_root: &Path,
            factory: Arc<dyn DaemonSessionFactory>,
            opts: DaemonOptions,
        ) -> Self {
            let idle_timeout_ms = opts.idle_timeout_ms.unwrap_or_else(resolve_idle_timeout_ms);
            Daemon {
                inner: Arc::new(DaemonInner {
                    project_root: project_root.to_path_buf(),
                    socket_path: get_daemon_socket_path(project_root),
                    pid_path: get_daemon_pid_path(project_root),
                    idle_timeout_ms,
                    signals_registered: opts.register_signal_handlers,
                    factory,
                    state: Mutex::new(DaemonState {
                        clients: HashMap::new(),
                        idle_deadline: None,
                        stopping: false,
                        stopped: false,
                    }),
                    cv: Condvar::new(),
                    next_client_id: AtomicU64::new(1),
                }),
            }
        }

        /// Bind the socket, kick off engine init, and register signal handlers.
        /// The lockfile body was already written atomically by
        /// [`try_acquire_daemon_lock`], so there is nothing to write here.
        /// Returns once the server is listening — the daemon then sticks around
        /// until idle/shutdown (block on [`Daemon::wait`]).
        pub fn start(&self) -> crate::error::Result<DaemonStartResult> {
            let inner = &self.inner;

            // Engine init is deliberately backgrounded — see #172.
            {
                let factory = Arc::clone(&inner.factory);
                let root = inner.project_root.clone();
                std::thread::spawn(move || factory.warm_up(&root));
            }

            // Stale socket file (left over from a SIGKILL'd previous daemon) will
            // wedge `bind` with EADDRINUSE. We arrived here holding the lockfile,
            // which means there's no live daemon, so it's safe to clear.
            let _ = fs::remove_file(&inner.socket_path);

            let listener = UnixListener::bind(&inner.socket_path).map_err(|e| {
                crate::error::CodeGraphError::other(format!(
                    "failed to bind daemon socket {}: {e}",
                    inner.socket_path.display()
                ))
            })?;
            // POSIX: tighten permissions to user-only — the socket lives under
            // `.codegraph/`, which is git-ignored but may be on a shared FS.
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&inner.socket_path, fs::Permissions::from_mode(0o600));
            }
            // Non-blocking so the accept loop can poll the stop flag + signals.
            let _ = listener.set_nonblocking(true);

            let lock = DaemonLockInfo {
                pid: std::process::id() as i64,
                version: CODEGRAPH_PACKAGE_VERSION.to_string(),
                socket_path: inner.socket_path.to_string_lossy().to_string(),
                started_at: now_ms(),
            };

            eprintln!(
                "[CodeGraph daemon] Listening on {} (pid {}, v{}). Idle timeout {}ms.",
                inner.socket_path.display(),
                std::process::id(),
                CODEGRAPH_PACKAGE_VERSION,
                inner.idle_timeout_ms
            );

            // No clients yet: arm the idle timer immediately so a daemon that
            // nobody ever connects to (e.g. spawned then abandoned because the
            // launcher died) doesn't pin resources forever.
            inner.arm_idle_timer();

            if inner.signals_registered {
                let handler = daemon_signal_handler as extern "C" fn(libc::c_int);
                unsafe {
                    libc::signal(libc::SIGINT, handler as usize as libc::sighandler_t);
                    libc::signal(libc::SIGTERM, handler as usize as libc::sighandler_t);
                }
            }

            // Idle-timeout monitor thread.
            {
                let inner = Arc::clone(&self.inner);
                std::thread::spawn(move || inner.idle_loop());
            }
            // Accept loop thread.
            {
                let inner = Arc::clone(&self.inner);
                std::thread::spawn(move || inner.accept_loop(listener));
            }

            Ok(DaemonStartResult {
                socket_path: inner.socket_path.clone(),
                lock,
            })
        }

        /// Currently-connected client count. Exposed for tests / status output.
        pub fn get_client_count(&self) -> usize {
            self.inner.state.lock().unwrap().clients.len()
        }

        /// The socket path the daemon is (or will be) listening on.
        pub fn get_socket_path(&self) -> PathBuf {
            self.inner.socket_path.clone()
        }

        /// Graceful shutdown: close all sessions, the engine, and clean up the
        /// lock. Idempotent. (TS ends with `process.exit(0)`; here the binary
        /// exits after [`Daemon::wait`] returns — see the struct-level note.)
        pub fn stop(&self, reason: &str) {
            self.inner.stop(reason);
        }

        /// SIGTERM disposition (#662): ignored while clients are attached
        /// (will exit via idle timeout), honored when idle. Public so the
        /// signal wiring/tests can drive it.
        pub fn handle_sigterm(&self) {
            self.inner.handle_sigterm();
        }

        /// True once [`Daemon::stop`] has completed.
        pub fn is_stopped(&self) -> bool {
            self.inner.state.lock().unwrap().stopped
        }

        /// Block until the daemon has fully stopped (idle timeout, signal, or
        /// an explicit [`Daemon::stop`]).
        pub fn wait(&self) {
            let mut st = self.inner.state.lock().unwrap();
            while !st.stopped {
                st = self.inner.cv.wait(st).unwrap();
            }
        }
    }

    impl DaemonInner {
        fn accept_loop(self: Arc<Self>, listener: UnixListener) {
            loop {
                if self.signals_registered {
                    if SIGINT_FLAG.swap(false, Ordering::SeqCst) {
                        self.stop("SIGINT");
                    }
                    if SIGTERM_FLAG.swap(false, Ordering::SeqCst) {
                        self.handle_sigterm();
                    }
                }
                if self.state.lock().unwrap().stopping {
                    break;
                }
                match listener.accept() {
                    Ok((stream, _addr)) => self.handle_connection(stream),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(25));
                    }
                    Err(_) => {
                        if self.state.lock().unwrap().stopping {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(25));
                    }
                }
            }
            // Listener drops here → socket fd closed.
        }

        fn handle_connection(self: &Arc<Self>, mut stream: UnixStream) {
            // The accepted stream must be blocking regardless of the listener's
            // non-blocking mode.
            let _ = stream.set_nonblocking(false);

            // Hello first so the proxy can verify versions before piping any
            // application bytes. The proxy reads exactly one line, then forwards.
            let hello = DaemonHello {
                codegraph: CODEGRAPH_PACKAGE_VERSION.to_string(),
                pid: std::process::id(),
                socket_path: self.socket_path.to_string_lossy().to_string(),
                protocol: 1,
            };
            let mut line = serde_json::to_string(&hello).unwrap_or_default();
            line.push('\n');
            if stream.write_all(line.as_bytes()).is_err() {
                return; // peer vanished between accept and hello
            }

            let id = self.next_client_id.fetch_add(1, Ordering::SeqCst);
            let registry_clone = match stream.try_clone() {
                Ok(c) => c,
                Err(_) => return,
            };
            {
                let mut st = self.state.lock().unwrap();
                if st.stopping {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    return;
                }
                st.clients.insert(id, registry_clone);
                // disarmIdleTimer()
                st.idle_deadline = None;
                self.cv.notify_all();
            }

            let inner = Arc::clone(self);
            std::thread::spawn(move || {
                inner.factory.serve_connection(stream, &inner.project_root);
                inner.drop_client(id);
            });
        }

        fn drop_client(&self, id: u64) {
            let mut st = self.state.lock().unwrap();
            if st.clients.remove(&id).is_none() {
                return;
            }
            if st.clients.is_empty() && !st.stopping {
                self.arm_idle_deadline(&mut st);
            }
            self.cv.notify_all();
        }

        fn handle_sigterm(&self) {
            {
                let st = self.state.lock().unwrap();
                if !st.clients.is_empty() {
                    eprintln!(
                        "[CodeGraph daemon] Ignoring SIGTERM while {} client(s) are attached; will exit via idle timeout.",
                        st.clients.len()
                    );
                    return;
                }
            }
            self.stop("SIGTERM");
        }

        fn arm_idle_timer(&self) {
            let mut st = self.state.lock().unwrap();
            if st.stopping {
                return;
            }
            self.arm_idle_deadline(&mut st);
            self.cv.notify_all();
        }

        fn arm_idle_deadline(&self, st: &mut DaemonState) {
            if st.idle_deadline.is_some() || st.stopping {
                return;
            }
            if self.idle_timeout_ms == 0 {
                return; // 0 = never idle-exit
            }
            st.idle_deadline = Some(Instant::now() + Duration::from_millis(self.idle_timeout_ms));
        }

        /// Idle-timeout monitor — the Rust equivalent of the TS unref'd
        /// `setTimeout`. Sleeps on the condvar until a deadline exists, then
        /// until it expires; re-arms instead of stopping if a connection landed
        /// in the meantime (the TS "last-second sanity check").
        fn idle_loop(self: Arc<Self>) {
            let mut st = self.state.lock().unwrap();
            loop {
                if st.stopping {
                    return;
                }
                match st.idle_deadline {
                    None => {
                        st = self.cv.wait(st).unwrap();
                    }
                    Some(deadline) => {
                        let now = Instant::now();
                        if now >= deadline {
                            if !st.clients.is_empty() {
                                // A connection landed between the timer firing
                                // and now — don't exit; re-arm.
                                st.idle_deadline = None;
                                self.arm_idle_deadline(&mut st);
                                continue;
                            }
                            drop(st);
                            self.stop("idle timeout");
                            return;
                        }
                        let (guard, _timeout) = self.cv.wait_timeout(st, deadline - now).unwrap();
                        st = guard;
                    }
                }
            }
        }

        fn stop(&self, reason: &str) {
            {
                let mut st = self.state.lock().unwrap();
                if st.stopping {
                    return;
                }
                st.stopping = true;
                // disarm idle timer
                st.idle_deadline = None;
                eprintln!(
                    "[CodeGraph daemon] Shutting down ({reason}; clients={}).",
                    st.clients.len()
                );
                for (_, stream) in st.clients.drain() {
                    let _ = stream.shutdown(std::net::Shutdown::Both); // best-effort session stop
                }
                self.cv.notify_all();
            }
            // The accept loop notices `stopping` within its poll interval and
            // drops the listener; no join here — stop() may be called *from*
            // that very thread (signal dispatch).
            self.factory.stop_engine();
            self.cleanup_lockfile();
            let _ = fs::remove_file(&self.socket_path); // may already be gone
            {
                let mut st = self.state.lock().unwrap();
                st.stopped = true;
                self.cv.notify_all();
            }
        }

        fn cleanup_lockfile(&self) {
            // Only remove if it still belongs to us — another daemon may have
            // already taken over while we were shutting down (extremely rare).
            if let Ok(raw) = fs::read_to_string(&self.pid_path) {
                if let Some(info) = decode_lock_info(&raw) {
                    if info.pid == std::process::id() as i64 {
                        let _ = fs::remove_file(&self.pid_path);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_timeout_parsing_mirrors_ts() {
        assert_eq!(parse_idle_timeout_ms(None), DEFAULT_IDLE_TIMEOUT_MS);
        assert_eq!(parse_idle_timeout_ms(Some("")), DEFAULT_IDLE_TIMEOUT_MS);
        assert_eq!(parse_idle_timeout_ms(Some("abc")), DEFAULT_IDLE_TIMEOUT_MS);
        assert_eq!(parse_idle_timeout_ms(Some("-1")), DEFAULT_IDLE_TIMEOUT_MS);
        assert_eq!(parse_idle_timeout_ms(Some("800")), 800);
        assert_eq!(parse_idle_timeout_ms(Some(" 800 ")), 800); // Number() trims
        assert_eq!(parse_idle_timeout_ms(Some("1e3")), 1000);
        assert_eq!(parse_idle_timeout_ms(Some("1500.9")), 1500); // floored
        assert_eq!(parse_idle_timeout_ms(Some("0")), 0); // 0 = never idle-exit
    }

    #[test]
    fn hello_serializes_with_camel_case_keys_in_ts_order() {
        let hello = DaemonHello {
            codegraph: "1.2.3".into(),
            pid: 42,
            socket_path: "/tmp/d.sock".into(),
            protocol: 1,
        };
        assert_eq!(
            serde_json::to_string(&hello).unwrap(),
            r#"{"codegraph":"1.2.3","pid":42,"socketPath":"/tmp/d.sock","protocol":1}"#
        );
    }
}

//! CodeGraph MCP Server (port of `src/mcp/index.ts`).
//!
//! Model Context Protocol server that exposes CodeGraph functionality
//! as tools for AI assistants like Claude.
//!
//! ```no_run
//! use codegraph::mcp::MCPServer;
//!
//! let server = MCPServer::new(Some("/path/to/project"));
//! server.start().unwrap(); // blocks for the lifetime of the session
//! ```
//!
//! Runtime modes (decided in [`MCPServer::start`]):
//!
//! - **Direct** — one process serves one MCP client over stdio. The pre-#411
//!   behavior; used when the user opts out (`CODEGRAPH_NO_DAEMON=1`), no
//!   `.codegraph/` is reachable, or the daemon machinery fails for any
//!   reason. The only mode available on Windows (no named-pipe daemon yet —
//!   see notes/mcp-daemon.md).
//! - **Proxy** — what an MCP host actually talks to when sharing is on: a
//!   thin stdio↔socket pipe to the shared daemon. The proxy carries the #277
//!   PPID watchdog, so a SIGKILL'd host reaps its proxy promptly. See
//!   `proxy.rs`.
//! - **Daemon** — a *detached* background process (its own session/process
//!   group) that serves N proxies over a Unix-domain socket, sharing one
//!   CodeGraph + watcher + SQLite handle. Spawned on demand; never a child of
//!   any host, so it survives individual sessions and is reaped by
//!   client-refcount + idle timeout. See `daemon.rs` and issue #411.
//!
//! The detached-daemon + always-proxy split is the fix for the review finding
//! that the original in-process daemon (a) was the first host's child, so
//! closing that terminal severed every other client, and (b) disabled the
//! PPID watchdog, regressing #277 (orphaned daemons on host SIGKILL).
//!
//! Deviation from TS: `start()` *blocks* for the lifetime of the server
//! (Node returned to an idle event loop; Rust has no loop to idle on). Exits
//! happen via `process::exit(0)` on stdin EOF / signals / watchdog — exactly
//! the TS exit points.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::directory::find_nearest_codegraph_root;
use crate::error::Result;
use crate::mcp::engine::{EngineHandle, MCPEngineOptions};
use crate::mcp::session::{MCPSession, MCPSessionOptions};
use crate::mcp::transport::{JsonRpcTransport, StdioTransport, StdioTransportOptions};

/// Env var that marks a process as the *detached daemon* itself (set by
/// `spawn_detached_daemon` when it re-invokes the CLI). Without it a
/// `serve --mcp` invocation is a launcher that connects-or-spawns; with it,
/// the process IS the daemon and must never try to spawn another (infinite
/// spawn).
const DAEMON_INTERNAL_ENV: &str = "CODEGRAPH_DAEMON_INTERNAL";

/// Retries for the detached daemon arbitrating the exclusive lock against a
/// racing sibling. Tiny — the lock resolves on the first round in practice;
/// the retries only cover clearing a genuinely stale (dead-pid) lockfile.
#[cfg(unix)]
const TAKEOVER_MAX_RETRIES: u32 = 5;
#[cfg(unix)]
const TAKEOVER_RETRY_DELAY_MS: u64 = 100;

/// How long a launcher waits for a freshly-spawned daemon to bind its socket
/// before giving up and running in-process. The daemon binds the socket
/// *before* the (backgrounded) engine warm-up, so this only needs to cover
/// process startup. Poll finely (25ms) so the proxy attaches the instant the
/// freshly-spawned daemon binds — same ~6s total give-up budget (240 × 25ms)
/// as TS, just finer granularity; socket-connect probes are cheap.
#[cfg(unix)]
const DAEMON_CONNECT_MAX_RETRIES: u32 = 240;
#[cfg(unix)]
const DAEMON_CONNECT_RETRY_DELAY_MS: u64 = 25;

/// Whether `CODEGRAPH_NO_DAEMON` was set to a truthy value.
fn daemon_opt_out_set() -> bool {
    let raw = match std::env::var("CODEGRAPH_NO_DAEMON") {
        Ok(v) if !v.is_empty() => v,
        _ => return false,
    };
    raw != "0" && raw.to_lowercase() != "false"
}

/// Whether this process was spawned to BE the detached daemon.
fn daemon_internal_set() -> bool {
    let raw = match std::env::var(DAEMON_INTERNAL_ENV) {
        Ok(v) if !v.is_empty() => v,
        _ => return false,
    };
    raw != "0" && raw.to_lowercase() != "false"
}

/// Resolve the project root the daemon machinery should key on. Returns
/// `None` when no `.codegraph/` is reachable from the candidate path — in
/// that case the caller must run in direct mode, since the daemon lockfile
/// and socket both live under `.codegraph/`.
///
/// The result is canonicalized so every client converges on the same
/// socket/lock path regardless of how it expressed the path: a client
/// launched with cwd under a symlink (e.g. macOS `/var` → `/private/var`)
/// and one that passed a symlinked `rootUri` would otherwise hash to
/// different sockets and silently fail to share the daemon.
fn resolve_daemon_root(explicit_path: Option<&str>) -> Option<PathBuf> {
    let candidate = match explicit_path {
        Some(p) => PathBuf::from(p),
        None => std::env::current_dir().ok()?,
    };
    let root = find_nearest_codegraph_root(&candidate)?;
    Some(std::fs::canonicalize(&root).unwrap_or(root))
}

/// MCP Server for CodeGraph.
///
/// Implements the Model Context Protocol to expose CodeGraph functionality as
/// tools that can be called by AI assistants.
///
/// Backwards-compatible constructor and `start()` signature with the
/// pre-issue-#411 implementation: callers continue to do
/// `MCPServer::new(path).start()`. Internally we now pick from direct /
/// proxy / daemon at start time.
pub struct MCPServer {
    project_path: Option<String>,
    /// Direct-mode-only state. In daemon mode the per-connection sessions
    /// live inside the Daemon; in proxy mode there is no session at all.
    session: Mutex<Option<MCPSession>>,
    engine: Mutex<Option<EngineHandle>>,
    /// Idempotency guard for `stop()`.
    stopped: AtomicBool,
}

impl MCPServer {
    pub fn new<P: Into<String>>(project_path: Option<P>) -> MCPServer {
        MCPServer {
            project_path: project_path.map(Into::into),
            session: Mutex::new(None),
            engine: Mutex::new(None),
            stopped: AtomicBool::new(false),
        }
    }

    /// Start the MCP server. Blocks for the lifetime of the session/daemon.
    ///
    /// Decision order:
    ///   1. `CODEGRAPH_DAEMON_INTERNAL=1` → we ARE the detached daemon; listen.
    ///   2. `CODEGRAPH_NO_DAEMON=1` → direct mode (unchanged pre-#411 behavior).
    ///   3. No `.codegraph/` reachable → direct mode (the daemon's lockfile
    ///      and socket both live under `.codegraph/`).
    ///   4. Otherwise connect to (or spawn) the shared daemon and proxy to it.
    ///
    /// On any unexpected failure in step 4 we transparently fall back to
    /// direct mode — a misbehaving daemon must never block a session from
    /// starting.
    pub fn start(&self) -> Result<()> {
        // The detached daemon process itself. Checked before the opt-out so
        // the daemon honors the same env it was spawned with (it never sets
        // NO_DAEMON).
        if daemon_internal_set() {
            #[cfg(unix)]
            {
                return self.start_daemon_process();
            }
            #[cfg(not(unix))]
            {
                // Named pipes unimplemented — never spawned on Windows, but
                // behave sanely if forced: serve in-process.
                return self.start_direct(
                    "CODEGRAPH_DAEMON_INTERNAL set on a platform without daemon support",
                );
            }
        }

        // Direct mode if the user opted out. Setting the env var is
        // sufficient to get the pre-#411 single-process behavior.
        if daemon_opt_out_set() {
            return self.start_direct("CODEGRAPH_NO_DAEMON set");
        }

        let root = match resolve_daemon_root(self.project_path.as_deref()) {
            Some(root) => root,
            None => {
                // No initialized project found — daemon mode has nowhere to
                // put its socket. The fresh-checkout / outside-project case;
                // behave as before.
                return self.start_direct("no .codegraph/ root found");
            }
        };

        #[cfg(unix)]
        {
            // Answer the MCP handshake LOCALLY (instant tool registration —
            // no waiting ~600ms for the daemon to spawn+bind, which produced
            // the cold-start race) and forward tool CALLS to the shared
            // daemon, connected in the background. Runs until the host
            // disconnects; the proxy installs its own watchdog and falls back
            // to an in-process engine if the daemon never comes up — so this
            // never returns.
            run_proxy_with_local_handshake(&root)
        }
        #[cfg(not(unix))]
        {
            let _ = root;
            // Windows: no Unix-domain socket / named-pipe daemon — run the
            // pre-#411 single-process path (as if CODEGRAPH_NO_DAEMON=1).
            self.start_direct("daemon mode unavailable on this platform")
        }
    }

    /// Stop the server. Mirrors the TS direct-mode behavior (close the
    /// session + engine, exit 0). Proxy mode never routes through here — the
    /// proxy exits itself; the daemon process exits via `Daemon::wait()`.
    pub fn stop(&self) {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(session) = self
            .session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            session.stop();
        }
        if let Some(engine) = self.engine.lock().unwrap_or_else(|e| e.into_inner()).take() {
            engine.stop();
        }
        std::process::exit(0);
    }

    /// Single-process stdio MCP session — the pre-issue-#411 code path.
    /// Blocks until the process exits (stdin EOF / signal / watchdog).
    fn start_direct(&self, reason: &str) -> Result<()> {
        if !reason.is_empty()
            && std::env::var("CODEGRAPH_MCP_DEBUG")
                .map(|v| !v.is_empty())
                .unwrap_or(false)
        {
            eprintln!("[CodeGraph MCP] Direct mode: {reason}.");
        }

        let engine = EngineHandle::spawn(MCPEngineOptions::default());
        let transport: Arc<StdioTransport> =
            Arc::new(StdioTransport::new(StdioTransportOptions::default()));
        let session = MCPSession::new(
            Arc::clone(&transport) as Arc<dyn JsonRpcTransport>,
            engine.clone(),
            MCPSessionOptions {
                explicit_project_path: self.project_path.clone(),
            },
        );

        if let Some(path) = &self.project_path {
            // Background init so the initialize response stays fast (#172).
            let _ = engine.ensure_initialized_async(path);
        }

        session.start();

        *self.engine.lock().unwrap_or_else(|e| e.into_inner()) = Some(engine.clone());
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(session);

        // Detect parent-process death (#277) — direct mode only. Daemon mode
        // is detached on purpose and reaps via idle timeout; proxy mode
        // installs its own watchdog inside the proxy. Stdin EOF already exits
        // via StdioTransport's exit-on-close, but SIGKILL of the parent
        // doesn't reliably close stdin on Linux.
        #[cfg(unix)]
        {
            let poll_ms = crate::mcp::proxy::parse_poll_ms(
                std::env::var("CODEGRAPH_PPID_POLL_MS").ok().as_deref(),
            );
            if poll_ms > 0 {
                let host_ppid = crate::mcp::proxy::parse_host_ppid(
                    std::env::var(crate::mcp::daemon_paths::HOST_PPID_ENV)
                        .ok()
                        .as_deref(),
                );
                let original_ppid = unsafe { libc::getppid() } as u32;
                let engine_for_watchdog = engine.clone();
                crate::mcp::proxy::spawn_ppid_watchdog_with(
                    poll_ms,
                    original_ppid,
                    host_ppid,
                    move |reason| {
                        eprintln!(
                            "[CodeGraph MCP] Parent process exited ({reason}); shutting down."
                        );
                        engine_for_watchdog.stop();
                        std::process::exit(0);
                    },
                );
            }
        }

        // Standard SIGINT/SIGTERM handlers that route to our `stop()`.
        #[cfg(unix)]
        install_direct_signal_handlers();

        // Block for the lifetime of the session. Exits:
        //   - stdin EOF → StdioTransport exit-on-close (process::exit(0));
        //   - SIGINT/SIGTERM → flag polled below → stop() → exit(0);
        //   - PPID watchdog → exit(0) above.
        loop {
            #[cfg(unix)]
            {
                if DIRECT_SIGNAL_FLAG.load(Ordering::SeqCst) {
                    self.stop();
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// Run as the detached shared daemon (process spawned with
    /// `CODEGRAPH_DAEMON_INTERNAL=1`). Arbitrate the exclusive lock, then
    /// either become the daemon (bind the socket, serve until idle/shutdown)
    /// or — if a live daemon already holds the lock — exit so we don't leak a
    /// redundant process.
    ///
    /// No PPID watchdog and no stdin handlers: the daemon is detached on
    /// purpose and reaps itself via client-refcount + idle timeout.
    #[cfg(unix)]
    fn start_daemon_process(&self) -> Result<()> {
        use crate::mcp::daemon::{
            AcquireResult,
            Daemon,
            DaemonOptions,
            clear_stale_daemon_lock,
            try_acquire_daemon_lock,
        };

        let root = resolve_daemon_root(self.project_path.as_deref())
            .or_else(|| self.project_path.as_deref().map(PathBuf::from))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"));

        for _attempt in 0..TAKEOVER_MAX_RETRIES {
            match try_acquire_daemon_lock(&root)? {
                AcquireResult::Acquired { .. } => {
                    let engine = EngineHandle::spawn(MCPEngineOptions::default());
                    let factory = Arc::new(EngineSessionFactory { engine });
                    let daemon = Daemon::new(&root, factory, DaemonOptions::default());
                    daemon.start()?;
                    // TS: the net.Server keeps the process alive; Rust blocks
                    // here, then exits (see notes/mcp-daemon.md wiring
                    // contract: bin must exit(0) after wait()).
                    daemon.wait();
                    std::process::exit(0);
                }
                AcquireResult::Taken { existing, pid_path } => {
                    // Taken. If the holder is alive, another daemon already
                    // serves (or is binding) — we're redundant; exit cleanly
                    // so the launcher proxies to it.
                    if let Some(info) = &existing {
                        if info.pid > 0 && crate::utils::is_process_alive(info.pid as u32) {
                            eprintln!(
                                "[CodeGraph daemon] Another daemon (pid {}) already holds the lock; exiting.",
                                info.pid
                            );
                            std::process::exit(0);
                        }
                    }
                    // Holder is dead (or the record is unreadable) — clear it
                    // (pid-verified, so we never delete a live daemon's lock)
                    // and retry the acquire.
                    clear_stale_daemon_lock(&pid_path, existing.as_ref().map(|e| e.pid));
                    std::thread::sleep(std::time::Duration::from_millis(TAKEOVER_RETRY_DELAY_MS));
                }
            }
        }

        eprintln!("[CodeGraph daemon] Could not acquire the daemon lock; exiting.");
        std::process::exit(0);
    }
}

// =============================================================================
// Direct-mode signal handling (unix)
// =============================================================================

#[cfg(unix)]
static DIRECT_SIGNAL_FLAG: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn direct_signal_handler(_sig: libc::c_int) {
    // Async-signal-safe: just set a flag; the start_direct loop polls it.
    DIRECT_SIGNAL_FLAG.store(true, Ordering::SeqCst);
}

#[cfg(unix)]
fn install_direct_signal_handlers() {
    let handler = direct_signal_handler as extern "C" fn(libc::c_int);
    unsafe {
        libc::signal(libc::SIGINT, handler as usize as libc::sighandler_t);
        libc::signal(libc::SIGTERM, handler as usize as libc::sighandler_t);
    }
}

// =============================================================================
// Daemon-side session factory (unix)
// =============================================================================

/// Implements the daemon's per-connection seam over the shared engine: one
/// engine thread serves every connected client (TS: one engine on the event
/// loop), each connection getting its own `MCPSession` + `SocketTransport`.
#[cfg(unix)]
struct EngineSessionFactory {
    engine: EngineHandle,
}

#[cfg(unix)]
impl crate::mcp::daemon::DaemonSessionFactory for EngineSessionFactory {
    fn warm_up(&self, project_root: &std::path::Path) {
        // TS: `void this.engine.ensureInitialized(root)` — runs on the
        // daemon's background warm-up thread, so blocking here is fine.
        self.engine
            .ensure_initialized(&project_root.to_string_lossy());
    }

    fn serve_connection(
        &self,
        stream: std::os::unix::net::UnixStream,
        project_root: &std::path::Path,
    ) {
        let transport = Arc::new(crate::mcp::transport::SocketTransport::new(stream));
        let session = MCPSession::new(
            Arc::clone(&transport) as Arc<dyn JsonRpcTransport>,
            self.engine.clone(),
            MCPSessionOptions {
                explicit_project_path: Some(project_root.to_string_lossy().to_string()),
            },
        );
        session.start();
        // Run the session to completion; returning drops the client
        // (refcount decrement happens in the daemon's accept loop).
        transport.wait_until_closed();
    }

    fn stop_engine(&self) {
        self.engine.stop();
    }
}

// =============================================================================
// Proxy mode (unix) — local handshake + shared-daemon forwarding
// =============================================================================

/// Spawn the shared daemon as a fully detached background process: its own
/// session (so a SIGHUP/SIGINT to the launcher's terminal can't reach it)
/// with stdio decoupled from the launcher (logs to `.codegraph/daemon.log`).
/// Re-invokes the same executable with `serve --mcp --path <root>` and
/// `CODEGRAPH_DAEMON_INTERNAL=1`. The spawned process self-arbitrates the
/// exclusive lock, so racing launchers may each spawn one — losers exit and
/// every launcher proxies through the single winner.
#[cfg(unix)]
fn spawn_detached_daemon(root: &std::path::Path) -> std::result::Result<(), String> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let exe = std::env::current_exe()
        .map_err(|_| "cannot resolve CLI script path to spawn the daemon".to_string())?;

    let mut cmd = Command::new(exe);
    cmd.arg("serve")
        .arg("--mcp")
        .arg("--path")
        .arg(root)
        .env(DAEMON_INTERNAL_ENV, "1")
        .stdin(Stdio::null());

    // Log to .codegraph/daemon.log; discard daemon output rather than fail.
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(crate::directory::get_codegraph_dir(root).join("daemon.log"))
        .ok();
    match log_file.and_then(|f| f.try_clone().ok().map(|c| (f, c))) {
        Some((f, c)) => {
            cmd.stdout(Stdio::from(f));
            cmd.stderr(Stdio::from(c));
        }
        None => {
            cmd.stdout(Stdio::null());
            cmd.stderr(Stdio::null());
        }
    }

    // Detach: new session (TS `detached: true`).
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }

    // Leak the Child (TS `child.unref()`); it self-arbitrates and exits on
    // its own. (A loser child lingers as a zombie until this proxy exits —
    // Node reaped via its SIGCHLD handler; harmless either way.)
    cmd.spawn().map_err(|e| e.to_string())?;
    Ok(())
}

/// Proxy mode (the common case). Serve the MCP handshake LOCALLY for instant
/// tool registration, forwarding tool calls to the shared daemon — which is
/// connected in the background (probed, then spawned + polled if absent) so
/// the handshake never waits ~600ms on it. Runs until the host disconnects;
/// the proxy falls back to an in-process engine if the daemon never binds, so
/// this never wedges a session. Never returns.
#[cfg(unix)]
fn run_proxy_with_local_handshake(root: &std::path::Path) -> ! {
    use crate::mcp::daemon_paths::get_daemon_socket_path;
    use crate::mcp::proxy::{
        DaemonSocket,
        HelloConnectResult,
        LocalHandshakeDeps,
        LocalToolExecutor,
        connect_with_hello,
        run_local_handshake_proxy,
    };
    use crate::mcp::version::CODEGRAPH_PACKAGE_VERSION;

    let socket_path = get_daemon_socket_path(root);
    let spawn_root = root.to_path_buf();
    let get_daemon_socket = Box::new(move || -> Option<DaemonSocket> {
        // Fast path: a daemon may already be listening.
        match connect_with_hello(&socket_path, CODEGRAPH_PACKAGE_VERSION) {
            HelloConnectResult::Connected(socket) => return Some(socket),
            // Definitive — serve in-process, don't poll for 6s; never run a
            // stale daemon against a newer client (or vice versa).
            HelloConnectResult::VersionMismatch => return None,
            HelloConnectResult::Unavailable => {}
        }
        // None reachable — spawn one (detached) and poll for its bind.
        if spawn_detached_daemon(&spawn_root).is_err() {
            return None; // the proxy serves this session in-process
        }
        for _attempt in 0..DAEMON_CONNECT_MAX_RETRIES {
            std::thread::sleep(std::time::Duration::from_millis(
                DAEMON_CONNECT_RETRY_DELAY_MS,
            ));
            match connect_with_hello(&socket_path, CODEGRAPH_PACKAGE_VERSION) {
                HelloConnectResult::Connected(socket) => return Some(socket),
                HelloConnectResult::VersionMismatch => return None,
                HelloConnectResult::Unavailable => {}
            }
        }
        None // never bound — the proxy serves this session in-process
    });

    /// In-process fallback executor over the shared engine — constructed
    /// lazily, ONLY if the daemon never comes up.
    struct EngineExecutor {
        engine: EngineHandle,
    }

    impl LocalToolExecutor for EngineExecutor {
        fn execute(
            &mut self,
            name: &str,
            arguments: &serde_json::Value,
        ) -> std::result::Result<serde_json::Value, String> {
            let result = self.engine.execute(name, arguments.clone());
            serde_json::to_value(&result).map_err(|e| e.to_string())
        }

        fn stop(&mut self) {
            self.engine.stop();
        }
    }

    let executor_root = root.to_path_buf();
    let make_local_executor = Box::new(move || -> Box<dyn LocalToolExecutor> {
        let engine = EngineHandle::spawn(MCPEngineOptions::default());
        // TS: `engine.ensureInitialized(root).catch(() => {/* degraded */})`
        // — backgrounded; the engine thread serializes the first execute
        // behind it.
        let _ = engine.ensure_initialized_async(&executor_root.to_string_lossy());
        Box::new(EngineExecutor { engine })
    });

    run_local_handshake_proxy(LocalHandshakeDeps {
        get_daemon_socket,
        make_local_executor,
        root: root.to_path_buf(),
        negotiate_protocol_version: Box::new(|v| {
            crate::mcp::session::negotiated_protocol_version(v)
        }),
        server_capabilities: crate::mcp::session::server_capabilities(),
        server_info: crate::mcp::session::server_info(),
        server_instructions: crate::mcp::server_instructions::SERVER_INSTRUCTIONS.to_string(),
        static_tools: Box::new(|| {
            serde_json::to_value(crate::mcp::tools::get_static_tools())
                .unwrap_or(serde_json::Value::Array(vec![]))
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_env_parsers_match_ts_truthiness() {
        // These read process env; exercise the pure string logic through a
        // local copy of the rule to avoid mutating env in parallel tests.
        let truthy = |raw: &str| !raw.is_empty() && raw != "0" && raw.to_lowercase() != "false";
        assert!(truthy("1"));
        assert!(truthy("true"));
        assert!(truthy("yes"));
        assert!(!truthy("0"));
        assert!(!truthy("false"));
        assert!(!truthy("FALSE"));
        assert!(!truthy(""));
    }

    #[test]
    fn resolve_daemon_root_finds_and_canonicalizes_the_project() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        std::fs::create_dir_all(root.join(".codegraph")).unwrap();
        // is_initialized requires the DB file too, not just the directory.
        std::fs::write(root.join(".codegraph").join("codegraph.db"), b"").unwrap();
        let nested = root.join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();

        let resolved = resolve_daemon_root(Some(&nested.to_string_lossy())).unwrap();
        assert_eq!(resolved, std::fs::canonicalize(&root).unwrap());

        let missing = tmp.path().join("no-project");
        std::fs::create_dir_all(&missing).unwrap();
        assert!(resolve_daemon_root(Some(&missing.to_string_lossy())).is_none());
    }
}

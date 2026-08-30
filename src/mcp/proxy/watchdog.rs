use std::io::Write;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::{is_process_alive_local, parse_host_ppid, parse_poll_ms};
use crate::mcp::daemon_paths::HOST_PPID_ENV;

fn current_ppid() -> u32 {
    // SAFETY: getppid has no preconditions and does not dereference memory.
    (unsafe { libc::getppid() }) as u32
}

pub struct ParentWatchdog {
    cancelled: Arc<AtomicBool>,
    cancel_on_drop: bool,
}

impl ParentWatchdog {
    fn detach(mut self) {
        self.cancel_on_drop = false;
    }
}

impl Drop for ParentWatchdog {
    fn drop(&mut self) {
        if self.cancel_on_drop {
            self.cancelled.store(true, Ordering::SeqCst);
        }
    }
}

pub fn start_ppid_watchdog_with(
    poll_ms: u64,
    original_ppid: u32,
    host_ppid: Option<u32>,
    on_death: impl FnOnce(&str) + Send + 'static,
) -> Option<ParentWatchdog> {
    if poll_ms == 0 {
        return None;
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    let thread_cancelled = Arc::clone(&cancelled);
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(poll_ms));
            if thread_cancelled.load(Ordering::SeqCst) {
                return;
            }
            let current = current_ppid();
            let ppid_changed = current != original_ppid;
            let host_gone = matches!(host_ppid, Some(host) if !is_process_alive_local(host));
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
    Some(ParentWatchdog {
        cancelled,
        cancel_on_drop: true,
    })
}

pub fn spawn_ppid_watchdog_with(
    poll_ms: u64,
    original_ppid: u32,
    host_ppid: Option<u32>,
    on_death: impl FnOnce(&str) + Send + 'static,
) {
    if let Some(watchdog) = start_ppid_watchdog_with(poll_ms, original_ppid, host_ppid, on_death) {
        watchdog.detach();
    }
}

pub fn start_command_supervision(label: &str) -> Option<ParentWatchdog> {
    let poll_ms = parse_poll_ms(std::env::var("CODEGRAPH_PPID_POLL_MS").ok().as_deref());
    let host_ppid = parse_host_ppid(std::env::var(HOST_PPID_ENV).ok().as_deref());
    let label = label.to_string();
    start_ppid_watchdog_with(poll_ms, current_ppid(), host_ppid, move |reason| {
        let _ = writeln!(
            std::io::stderr().lock(),
            "[CodeGraph {label}] Parent process exited ({reason}); aborting."
        );
        std::process::exit(1);
    })
}

pub(super) fn start_ppid_watchdog(stream: &UnixStream) {
    let poll_ms = parse_poll_ms(std::env::var("CODEGRAPH_PPID_POLL_MS").ok().as_deref());
    if poll_ms == 0 {
        return;
    }
    let host_ppid = parse_host_ppid(std::env::var(HOST_PPID_ENV).ok().as_deref());
    let socket = stream.try_clone().ok();
    spawn_ppid_watchdog_with(poll_ms, current_ppid(), host_ppid, move |reason| {
        eprintln!("[CodeGraph MCP] Parent process exited ({reason}); shutting down.");
        if let Some(socket) = socket {
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
        std::process::exit(0);
    });
}

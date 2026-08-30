//! Version-checked byte proxy between MCP stdio and the shared daemon socket.

use crate::mcp::version::CODEGRAPH_PACKAGE_VERSION;

pub const DEFAULT_PPID_POLL_MS: u64 = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyOutcome {
    Proxied,
    FallbackNeeded,
}

#[derive(Debug)]
pub struct ProxyResult {
    pub outcome: ProxyOutcome,
    pub reason: Option<String>,
}

pub fn parse_poll_ms(raw: Option<&str>) -> u64 {
    let raw = match raw {
        None | Some("") => return DEFAULT_PPID_POLL_MS,
        Some(raw) => raw,
    };
    let parsed: f64 = match raw.trim().parse() {
        Ok(value) => value,
        Err(_) => return DEFAULT_PPID_POLL_MS,
    };
    if !parsed.is_finite() || parsed < 0.0 {
        return DEFAULT_PPID_POLL_MS;
    }
    parsed.floor() as u64
}

pub fn parse_host_ppid(raw: Option<&str>) -> Option<u32> {
    let raw = raw?;
    if raw.is_empty() {
        return None;
    }
    let parsed: f64 = raw.trim().parse().ok()?;
    if !parsed.is_finite() || parsed.fract() != 0.0 || parsed <= 1.0 || parsed > u32::MAX as f64 {
        return None;
    }
    Some(parsed as u32)
}

fn is_process_alive_local(pid: u32) -> bool {
    crate::utils::is_process_alive(pid)
}

#[cfg(unix)]
mod connection;
#[cfg(unix)]
mod watchdog;

#[cfg(unix)]
pub(crate) use connection::connect_with_hello_until;
#[cfg(unix)]
pub use connection::{
    DaemonSocket,
    HelloConnectResult,
    connect_with_hello,
    run_connected_proxy,
    run_proxy,
};
#[cfg(unix)]
pub use watchdog::{spawn_ppid_watchdog_with, start_command_supervision};

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::*;

    #[test]
    fn parse_poll_ms_mirrors_ts() {
        assert_eq!(parse_poll_ms(None), DEFAULT_PPID_POLL_MS);
        assert_eq!(parse_poll_ms(Some("")), DEFAULT_PPID_POLL_MS);
        assert_eq!(parse_poll_ms(Some("abc")), DEFAULT_PPID_POLL_MS);
        assert_eq!(parse_poll_ms(Some("-5")), DEFAULT_PPID_POLL_MS);
        assert_eq!(parse_poll_ms(Some("0")), 0);
        assert_eq!(parse_poll_ms(Some("200")), 200);
        assert_eq!(parse_poll_ms(Some("250.9")), 250);
    }

    #[test]
    fn parse_host_ppid_mirrors_ts() {
        assert_eq!(parse_host_ppid(None), None);
        assert_eq!(parse_host_ppid(Some("")), None);
        assert_eq!(parse_host_ppid(Some("abc")), None);
        assert_eq!(parse_host_ppid(Some("0")), None);
        assert_eq!(parse_host_ppid(Some("1")), None);
        assert_eq!(parse_host_ppid(Some("12.5")), None);
        assert_eq!(parse_host_ppid(Some("2")), Some(2));
        assert_eq!(parse_host_ppid(Some("1e3")), Some(1000));
    }

    #[cfg(unix)]
    #[test]
    fn dropping_a_parent_watchdog_cancels_its_callback() {
        // Given: a cancellable watchdog points at a dead host.
        let fired = Arc::new(AtomicBool::new(false));
        let callback_fired = Arc::clone(&fired);
        let watchdog = watchdog::start_ppid_watchdog_with(
            50,
            std::process::id(),
            Some(u32::MAX - 1),
            move |_| callback_fired.store(true, Ordering::SeqCst),
        )
        .expect("watchdog enabled");

        // When: normal completion drops the guard before its first poll.
        drop(watchdog);
        std::thread::sleep(Duration::from_millis(100));

        // Then: cleanup suppresses the parent-death callback.
        assert!(!fired.load(Ordering::SeqCst));
    }
}

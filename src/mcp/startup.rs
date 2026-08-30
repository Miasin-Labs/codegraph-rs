use std::pin::Pin;
use std::sync::mpsc::{self, Sender};
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, ReadBuf};

pub const DEFAULT_STARTUP_HANDSHAKE_TIMEOUT_MS: u64 = 900_000;
pub const STARTUP_HANDSHAKE_TIMEOUT_ENV: &str = "CODEGRAPH_STARTUP_HANDSHAKE_TIMEOUT_MS";

pub fn parse_startup_handshake_timeout_ms(raw: Option<&str>) -> u64 {
    let Some(raw) = raw else {
        return DEFAULT_STARTUP_HANDSHAKE_TIMEOUT_MS;
    };
    if raw.is_empty() {
        return DEFAULT_STARTUP_HANDSHAKE_TIMEOUT_MS;
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return 0;
    }
    let Ok(parsed) = trimmed.parse::<f64>() else {
        return DEFAULT_STARTUP_HANDSHAKE_TIMEOUT_MS;
    };
    if !parsed.is_finite() {
        return DEFAULT_STARTUP_HANDSHAKE_TIMEOUT_MS;
    }
    if parsed <= 0.0 {
        return 0;
    }
    parsed.floor() as u64
}

fn configured_timeout() -> Duration {
    Duration::from_millis(parse_startup_handshake_timeout_ms(
        std::env::var(STARTUP_HANDSHAKE_TIMEOUT_ENV).ok().as_deref(),
    ))
}

pub(crate) fn arm_process_timeout(
    on_abandoned: impl FnOnce() + Send + 'static,
) -> Option<Sender<()>> {
    let timeout = configured_timeout();
    if timeout.is_zero() {
        return None;
    }
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        if receiver.recv_timeout(timeout) == Err(mpsc::RecvTimeoutError::Timeout) {
            eprintln!(
                "[CodeGraph MCP] No MCP traffic since startup; assuming an abandoned launch and shutting down (#1185). Tune with CODEGRAPH_STARTUP_HANDSHAKE_TIMEOUT_MS (0 disables)."
            );
            on_abandoned();
        }
    });
    Some(sender)
}

pub(crate) struct StartupRead<R> {
    reader: R,
    disarm: Option<Sender<()>>,
}

impl<R> StartupRead<R> {
    pub(crate) fn new(reader: R, disarm: Option<Sender<()>>) -> Self {
        Self { reader, disarm }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for StartupRead<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buffer.filled().len();
        let result = Pin::new(&mut self.reader).poll_read(context, buffer);
        if matches!(result, Poll::Ready(Ok(()))) && buffer.filled().len() > before {
            if let Some(disarm) = self.disarm.take() {
                let _ = disarm.send(());
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_timeout_parse_matches_the_source_contract() {
        assert_eq!(
            parse_startup_handshake_timeout_ms(None),
            DEFAULT_STARTUP_HANDSHAKE_TIMEOUT_MS
        );
        assert_eq!(
            parse_startup_handshake_timeout_ms(Some("")),
            DEFAULT_STARTUP_HANDSHAKE_TIMEOUT_MS
        );
        assert_eq!(
            parse_startup_handshake_timeout_ms(Some("abc")),
            DEFAULT_STARTUP_HANDSHAKE_TIMEOUT_MS
        );
        assert_eq!(
            parse_startup_handshake_timeout_ms(Some("NaN")),
            DEFAULT_STARTUP_HANDSHAKE_TIMEOUT_MS
        );
        assert_eq!(parse_startup_handshake_timeout_ms(Some("  ")), 0);
        assert_eq!(parse_startup_handshake_timeout_ms(Some("0")), 0);
        assert_eq!(parse_startup_handshake_timeout_ms(Some("-5")), 0);
        assert_eq!(parse_startup_handshake_timeout_ms(Some("2500.7")), 2500);
        assert_eq!(parse_startup_handshake_timeout_ms(Some("1e30")), u64::MAX);
    }
}

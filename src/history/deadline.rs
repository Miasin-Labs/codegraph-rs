//! A hard deadline on one read-only connection: past it, SQLite interrupts
//! the statement that is running. Readers check [`Deadline::expired`]
//! between steps — an interrupt only stops a statement already running.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use rusqlite::Connection;

/// Interrupts `conn` once `after` has passed, unless dropped first.
pub(crate) struct Deadline {
    done: Option<mpsc::Sender<()>>,
    watchdog: Option<JoinHandle<()>>,
    fired: Arc<AtomicBool>,
}

impl Deadline {
    pub(crate) fn arm(conn: &Connection, after: Duration) -> Self {
        let interrupt = conn.get_interrupt_handle();
        let fired = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&fired);
        let (done, wait) = mpsc::channel::<()>();
        let watchdog = std::thread::spawn(move || {
            if wait.recv_timeout(after) == Err(mpsc::RecvTimeoutError::Timeout) {
                flag.store(true, Ordering::SeqCst);
                interrupt.interrupt();
            }
        });
        Self {
            done: Some(done),
            watchdog: Some(watchdog),
            fired,
        }
    }

    /// The deadline passed (later queries should not start).
    pub(crate) fn expired(&self) -> bool {
        self.fired.load(Ordering::SeqCst)
    }
}

impl Drop for Deadline {
    fn drop(&mut self) {
        // A dropped sender wakes the watchdog without interrupting.
        drop(self.done.take());
        if let Some(watchdog) = self.watchdog.take() {
            let _ = watchdog.join();
        }
    }
}

/// `err` is SQLite's "interrupted" (the deadline stopped a statement).
pub(crate) fn is_interrupt(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(e, _) if e.code == rusqlite::ErrorCode::OperationInterrupted
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unexpired_deadline_never_interrupts() {
        let conn = Connection::open_in_memory().unwrap();
        let deadline = Deadline::arm(&conn, Duration::from_secs(30));
        let n: i64 = conn.query_row("SELECT 41 + 1", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 42);
        assert!(!deadline.expired());
    }

    #[test]
    fn a_long_query_is_interrupted_at_the_deadline() {
        let conn = Connection::open_in_memory().unwrap();
        let deadline = Deadline::arm(&conn, Duration::from_millis(20));
        let err = conn
            .query_row(
                "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c) SELECT MAX(x) FROM c",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_err();
        assert!(is_interrupt(&err), "{err}");
        assert!(deadline.expired());
    }
}

//! One request's wall-clock budget across every graph it reads.
//!
//! Readers check [`Deadline::expired`] before each step (opening a graph,
//! reading a project); a statement already running when the budget runs
//! out is interrupted by a watchdog thread, started the first time a
//! connection is [`watch`](Deadline::watch)ed, that interrupts every
//! watched connection at once. An interrupted read fails with SQLite's
//! "interrupted" error, which callers treat as "out of time", never as a
//! fault.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rusqlite::{Connection, InterruptHandle};

/// The budget of one request.
pub struct Deadline {
    end: Instant,
    fired: Arc<AtomicBool>,
    watchdog: RefCell<Option<Watchdog>>,
}

struct Watchdog {
    handles: Arc<Mutex<Vec<InterruptHandle>>>,
    done: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Deadline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Deadline")
            .field("remaining", &self.remaining())
            .field("expired", &self.expired())
            .finish()
    }
}

impl Deadline {
    /// A budget of `budget` from now.
    pub fn new(budget: Duration) -> Self {
        Self {
            end: Instant::now() + budget,
            fired: Arc::new(AtomicBool::new(false)),
            watchdog: RefCell::new(None),
        }
    }

    /// The budget is spent: start nothing new.
    pub fn expired(&self) -> bool {
        self.fired.load(Ordering::SeqCst) || Instant::now() >= self.end
    }

    pub fn remaining(&self) -> Duration {
        self.end.saturating_duration_since(Instant::now())
    }

    /// Interrupt `conn`'s running statement once the budget is spent.
    pub fn watch(&self, conn: &Connection) {
        let handle = conn.get_interrupt_handle();
        let mut slot = self.watchdog.borrow_mut();
        let handles = Arc::clone(&slot.get_or_insert_with(|| Watchdog::start(self)).handles);
        drop(slot);
        if let Ok(mut handles) = handles.lock() {
            handles.push(handle);
        };
    }
}

impl Watchdog {
    fn start(deadline: &Deadline) -> Self {
        let handles: Arc<Mutex<Vec<InterruptHandle>>> = Arc::new(Mutex::new(Vec::new()));
        let (done, wait) = mpsc::channel::<()>();
        let fired = Arc::clone(&deadline.fired);
        let watched = Arc::clone(&handles);
        let after = deadline.remaining();
        let thread = std::thread::Builder::new()
            .name("codegraph-federation-deadline".into())
            .spawn(move || {
                if wait.recv_timeout(after) == Err(mpsc::RecvTimeoutError::Timeout) {
                    fired.store(true, Ordering::SeqCst);
                    if let Ok(handles) = watched.lock() {
                        for handle in handles.iter() {
                            handle.interrupt();
                        }
                    }
                }
            })
            .ok();
        Self {
            handles,
            done: Some(done),
            thread,
        }
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        // A dropped sender wakes the watchdog without interrupting.
        drop(self.done.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// `err` is SQLite's "interrupted" (the deadline stopped a statement).
pub(crate) fn is_interrupt(err: &crate::error::CodeGraphError) -> bool {
    matches!(
        err,
        crate::error::CodeGraphError::Sqlite(rusqlite::Error::SqliteFailure(e, _))
            if e.code == rusqlite::ErrorCode::OperationInterrupted
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unspent_budget_never_interrupts() {
        let conn = Connection::open_in_memory().unwrap();
        let deadline = Deadline::new(Duration::from_secs(30));
        deadline.watch(&conn);
        let n: i64 = conn.query_row("SELECT 41 + 1", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 42);
        assert!(!deadline.expired());
    }

    #[test]
    fn a_running_statement_is_interrupted_when_the_budget_is_spent() {
        let first = Connection::open_in_memory().unwrap();
        let second = Connection::open_in_memory().unwrap();
        let deadline = Deadline::new(Duration::from_millis(30));
        deadline.watch(&first);
        deadline.watch(&second);
        let endless =
            "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c) SELECT MAX(x) FROM c";
        let err = second
            .query_row(endless, [], |r| r.get::<_, i64>(0))
            .unwrap_err();
        assert!(err.to_string().contains("interrupted"), "{err}");
        assert!(deadline.expired());
    }
}

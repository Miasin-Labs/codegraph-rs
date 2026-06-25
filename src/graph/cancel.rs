//! Cooperative cancellation for graph traversals.
//!
//! Graph walks (`find_path`, BFS/DFS, impact, type hierarchy) run on the single
//! MCP engine thread and can scan the whole index. A client
//! `notifications/cancelled` flips an `AtomicBool` in the per-call
//! [`CallContext`](crate::mcp::tools); to let an *in-flight* traversal observe
//! that without threading a token through every traversal signature (and every
//! call site and test), the dispatcher installs that flag into a thread-local
//! for the duration of one `tools/call`, and the traversal loops poll it via
//! [`check`] at each iteration / recursion head.
//!
//! This is sound because the engine is single-threaded and holds a `!Send`
//! `Rc<CodeGraph>`: only one tool call runs on this thread at a time, and the
//! [`CancelGuard`] restores the previous token on drop, so a flag can never
//! leak into a later call. When no token is installed (the CLI, tests, or a
//! call the client never cancels) [`is_cancelled`] is `false` and traversal
//! behavior is unchanged.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{CodeGraphError, Result};

thread_local! {
    /// The current tool call's cancellation flag, if it carried one.
    static CANCEL: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
}

/// RAII handle that installs `flag` as the current thread's cancellation token
/// and restores the previously installed token on drop. Hold it for the
/// duration of one tool call's dispatch.
#[must_use = "the token is uninstalled as soon as the guard is dropped"]
pub struct CancelGuard {
    prev: Option<Arc<AtomicBool>>,
}

impl CancelGuard {
    /// Install `flag` (or clear, with `None`) as the current-thread token.
    pub fn install(flag: Option<Arc<AtomicBool>>) -> CancelGuard {
        let prev = CANCEL.with(|c| c.replace(flag));
        CancelGuard { prev }
    }
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        let prev = self.prev.take();
        CANCEL.with(|c| *c.borrow_mut() = prev);
    }
}

/// Whether the current tool call has been cancelled by the client.
#[inline]
pub fn is_cancelled() -> bool {
    CANCEL.with(|c| {
        c.borrow()
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
    })
}

/// `Err` if the current tool call was cancelled, so traversal loops can bail
/// out with `?`. The message is matched by the dispatcher / suppressed by the
/// session for a cancelled request.
#[inline]
pub fn check() -> Result<()> {
    if is_cancelled() {
        Err(CodeGraphError::other("Traversal cancelled by client"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_token_means_not_cancelled() {
        assert!(!is_cancelled());
        assert!(check().is_ok());
    }

    #[test]
    fn guard_reflects_flag_and_restores_on_drop() {
        let flag = Arc::new(AtomicBool::new(false));
        {
            let _g = CancelGuard::install(Some(Arc::clone(&flag)));
            assert!(!is_cancelled());
            flag.store(true, Ordering::Relaxed);
            assert!(is_cancelled());
            assert!(check().is_err());
        }
        // Token uninstalled on drop — back to the default for this thread.
        assert!(!is_cancelled());
    }

    #[test]
    fn nested_guards_restore_the_outer_token() {
        let outer = Arc::new(AtomicBool::new(true));
        let inner = Arc::new(AtomicBool::new(false));
        let _o = CancelGuard::install(Some(Arc::clone(&outer)));
        assert!(is_cancelled());
        {
            let _i = CancelGuard::install(Some(Arc::clone(&inner)));
            assert!(!is_cancelled());
        }
        // Inner guard dropped: the outer (cancelled) token is restored.
        assert!(is_cancelled());
    }
}

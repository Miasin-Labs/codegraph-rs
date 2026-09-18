//! The deduplicating dirty set both solvers drain.

use std::collections::VecDeque;

/// FIFO of pending indices in `0..len` that holds each index at most once.
///
/// Pushing an index that is already pending is a no-op, so a burst of
/// changes to one input re-runs each dependent once rather than once per
/// change.
#[derive(Debug)]
pub(super) struct DirtySet {
    queue: VecDeque<usize>,
    pending: Vec<bool>,
}

impl DirtySet {
    /// A set with every index in `0..len` pending, in ascending order.
    pub(super) fn all(len: usize) -> Self {
        Self {
            queue: (0..len).collect(),
            pending: vec![true; len],
        }
    }

    /// Queue `idx` unless it is already pending. `idx` must be `< len`;
    /// callers only push indices of the body or function list the set was
    /// built for.
    pub(super) fn push(&mut self, idx: usize) {
        if !std::mem::replace(&mut self.pending[idx], true) {
            self.queue.push_back(idx);
        }
    }

    /// Take the oldest pending index.
    pub(super) fn pop(&mut self) -> Option<usize> {
        let idx = self.queue.pop_front()?;
        self.pending[idx] = false;
        Some(idx)
    }
}

#[cfg(test)]
mod tests {
    use super::DirtySet;

    #[test]
    fn pending_index_is_queued_once() {
        let mut dirty = DirtySet::all(3);
        assert_eq!(dirty.pop(), Some(0));
        dirty.push(2); // still pending from `all`
        dirty.push(0);
        dirty.push(0);
        let drained: Vec<usize> = std::iter::from_fn(|| dirty.pop()).collect();
        assert_eq!(drained, [1, 2, 0]);
    }
}

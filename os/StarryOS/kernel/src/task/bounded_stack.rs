//! Fixed-capacity task state used from non-blocking observer paths.

use alloc::vec::Vec;

/// A preallocated LIFO stack that never grows after construction.
pub(crate) struct BoundedStack<T, const CAPACITY: usize> {
    entries: Vec<T>,
}

impl<T, const CAPACITY: usize> BoundedStack<T, CAPACITY> {
    /// Creates an empty stack with storage for its complete lifetime capacity.
    pub(crate) fn new() -> Self {
        assert!(CAPACITY != 0, "bounded stack capacity must be non-zero");
        Self {
            entries: Vec::with_capacity(CAPACITY),
        }
    }

    /// Pushes one entry without growing the backing allocation.
    ///
    /// Returns ownership of `entry` when the fixed capacity is exhausted.
    pub(crate) fn try_push(&mut self, entry: T) -> Result<(), T> {
        if self.entries.len() == CAPACITY {
            return Err(entry);
        }
        self.entries.push(entry);
        Ok(())
    }

    /// Pops the most recently inserted entry.
    pub(crate) fn pop(&mut self) -> Option<T> {
        self.entries.pop()
    }
}

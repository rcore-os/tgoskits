//! Wait queue storage core.

use alloc::collections::VecDeque;

/// FIFO wait queue storage.
///
/// This type intentionally does not block, wake, or change task state by
/// itself. OS adapters hold the appropriate scheduler/wait locks and call into
/// this storage core for queue ordering.
pub struct WaitQueueCore<T> {
    queue: VecDeque<T>,
}

impl<T> WaitQueueCore<T> {
    /// Creates an empty wait queue core.
    pub const fn new() -> Self {
        Self {
            queue: VecDeque::new(),
        }
    }

    /// Pushes one waiter at the back.
    pub fn push_back(&mut self, waiter: T) {
        self.queue.push_back(waiter);
    }

    /// Pops one waiter from the front.
    pub fn pop_front(&mut self) -> Option<T> {
        self.queue.pop_front()
    }

    /// Retains waiters matching `keep`.
    pub fn retain(&mut self, mut keep: impl FnMut(&T) -> bool) {
        self.queue.retain(|waiter| keep(waiter));
    }

    /// Returns whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Returns the number of queued waiters.
    pub fn len(&self) -> usize {
        self.queue.len()
    }
}

impl<T> Default for WaitQueueCore<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::WaitQueueCore;

    #[test]
    fn wait_queue_core_preserves_fifo_and_retain() {
        let mut queue = WaitQueueCore::new();
        queue.push_back(1);
        queue.push_back(2);
        queue.push_back(3);

        queue.retain(|value| *value != 2);

        assert_eq!(queue.len(), 2);
        assert_eq!(queue.pop_front(), Some(1));
        assert_eq!(queue.pop_front(), Some(3));
        assert_eq!(queue.pop_front(), None);
        assert!(queue.is_empty());
    }
}

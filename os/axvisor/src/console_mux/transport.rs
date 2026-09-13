//! Fixed-capacity transport from atomic console producers to a task owner.

use core::mem;

use heapless::Deque;

/// Non-allocating byte queue for deferred host-console output.
///
/// Synchronization deliberately remains outside this type so the OS adapter
/// can choose the lock required by its producer contexts. A transaction that
/// does not fit is discarded in full, so backpressure never exposes a partial
/// control sequence or logical record. The task owner reports the discarded
/// byte count before writing the preserved stream.
#[derive(Debug)]
pub struct HostOutputQueue<const CAPACITY: usize> {
    bytes: Deque<u8, CAPACITY>,
    dropped_bytes: usize,
}

impl<const CAPACITY: usize> HostOutputQueue<CAPACITY> {
    /// Creates an empty queue.
    pub const fn new() -> Self {
        Self {
            bytes: Deque::new(),
            dropped_bytes: 0,
        }
    }

    /// Starts one non-allocating producer transaction.
    pub fn begin_transaction(&mut self) -> HostOutputTransaction<'_, CAPACITY> {
        HostOutputTransaction {
            queue: self,
            accepted_bytes: 0,
            source_bytes: 0,
            overflowed: false,
        }
    }

    /// Enqueues one complete producer transaction.
    pub fn enqueue(&mut self, bytes: &[u8]) {
        self.begin_transaction().enqueue(bytes);
    }

    /// Removes the oldest queued bytes into `output`.
    pub fn dequeue(&mut self, output: &mut [u8]) -> usize {
        let mut count = 0;
        for slot in output {
            let Some(byte) = self.bytes.pop_front() else {
                break;
            };
            *slot = byte;
            count += 1;
        }
        count
    }

    /// Takes the number of bytes discarded since the previous report.
    pub fn take_dropped_bytes(&mut self) -> usize {
        mem::take(&mut self.dropped_bytes)
    }
}

/// In-progress fixed-queue transaction.
///
/// Dropping this value commits every submitted byte when they all fit. On
/// overflow it removes the transaction's earlier chunks and accounts for the
/// complete source length instead.
pub struct HostOutputTransaction<'a, const CAPACITY: usize> {
    queue: &'a mut HostOutputQueue<CAPACITY>,
    accepted_bytes: usize,
    source_bytes: usize,
    overflowed: bool,
}

impl<const CAPACITY: usize> HostOutputTransaction<'_, CAPACITY> {
    /// Adds another contiguous chunk to this transaction.
    pub fn enqueue(&mut self, bytes: &[u8]) {
        self.source_bytes = self.source_bytes.saturating_add(bytes.len());
        if self.overflowed {
            return;
        }

        for &byte in bytes {
            if self.queue.bytes.push_back(byte).is_err() {
                self.overflowed = true;
                self.rollback();
                return;
            }
            self.accepted_bytes += 1;
        }
    }

    /// Returns whether the producer submitted bytes, including a transaction
    /// that will be reported as dropped.
    pub fn has_activity(&self) -> bool {
        self.source_bytes != 0
    }

    fn rollback(&mut self) {
        for _ in 0..self.accepted_bytes {
            self.queue
                .bytes
                .pop_back()
                .expect("accepted transaction bytes remain at the queue tail");
        }
        self.accepted_bytes = 0;
    }
}

impl<const CAPACITY: usize> Drop for HostOutputTransaction<'_, CAPACITY> {
    fn drop(&mut self) {
        if self.overflowed {
            self.queue.dropped_bytes = self.queue.dropped_bytes.saturating_add(self.source_bytes);
        }
    }
}

impl<const CAPACITY: usize> Default for HostOutputQueue<CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_fifo_order_without_overflow() {
        let mut queue = HostOutputQueue::<8>::new();
        queue.enqueue(b"abc");
        queue.enqueue(b"def");

        let mut output = [0; 8];
        assert_eq!(queue.dequeue(&mut output), 6);
        assert_eq!(&output[..6], b"abcdef");
        assert_eq!(queue.take_dropped_bytes(), 0);
    }

    #[test]
    fn overflow_preserves_queued_transactions_and_drops_the_new_one() {
        let mut queue = HostOutputQueue::<4>::new();
        queue.enqueue(b"abc");
        queue.enqueue(b"def");

        let mut output = [0; 4];
        assert_eq!(queue.dequeue(&mut output), 3);
        assert_eq!(&output[..3], b"abc");
        assert_eq!(queue.take_dropped_bytes(), 3);
        assert_eq!(queue.take_dropped_bytes(), 0);
    }

    #[test]
    fn oversized_transaction_is_dropped_in_full() {
        let mut queue = HostOutputQueue::<3>::new();
        queue.enqueue(b"12345");

        let mut output = [0; 3];
        assert_eq!(queue.dequeue(&mut output), 0);
        assert_eq!(queue.take_dropped_bytes(), 5);
    }

    #[test]
    fn streamed_overflow_rolls_back_every_chunk_of_the_transaction() {
        let mut queue = HostOutputQueue::<5>::new();
        queue.enqueue(b"old");
        {
            let mut transaction = queue.begin_transaction();
            transaction.enqueue(b"12");
            transaction.enqueue(b"34");
            assert!(transaction.has_activity());
        }

        let mut output = [0; 5];
        assert_eq!(queue.dequeue(&mut output), 3);
        assert_eq!(&output[..3], b"old");
        assert_eq!(queue.take_dropped_bytes(), 4);
    }
}

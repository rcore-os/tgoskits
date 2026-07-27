//! Small sequence counter for lockless read-mostly summaries.

use core::{
    hint::spin_loop,
    sync::atomic::{AtomicUsize, Ordering},
};

/// Sequence counter whose writers are serialized by an enclosing lock.
#[derive(Debug, Default)]
pub(crate) struct SequenceCounter {
    sequence: AtomicUsize,
}

impl SequenceCounter {
    /// Marks the beginning of a writer critical section.
    pub(crate) fn write_begin(&self) {
        let previous = self.sequence.fetch_add(1, Ordering::AcqRel);
        debug_assert_eq!(previous & 1, 0, "sequence writers must be serialized");
    }

    /// Publishes the completed writer critical section.
    pub(crate) fn write_end(&self) {
        let previous = self.sequence.fetch_add(1, Ordering::Release);
        debug_assert_eq!(previous & 1, 1, "sequence write must be active");
    }

    /// Starts a read snapshot after any active writer completes.
    pub(crate) fn read_begin(&self) -> usize {
        loop {
            let sequence = self.sequence.load(Ordering::Acquire);
            if sequence & 1 == 0 {
                return sequence;
            }
            spin_loop();
        }
    }

    /// Returns whether a read snapshot raced with a writer.
    pub(crate) fn read_retry(&self, start: usize) -> bool {
        self.sequence.load(Ordering::Acquire) != start
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_a_writer_between_snapshot_reads() {
        let sequence = SequenceCounter::default();
        let start = sequence.read_begin();
        sequence.write_begin();
        sequence.write_end();
        assert!(sequence.read_retry(start));
    }
}

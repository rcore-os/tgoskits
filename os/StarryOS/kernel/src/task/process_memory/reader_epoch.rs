//! Reader admission for a publication with externally serialized writers.

#[cfg(not(all(test, not(target_os = "none"))))]
use core::sync::atomic::{AtomicUsize, Ordering, fence};

#[cfg(all(test, not(target_os = "none")))]
use loom::sync::atomic::{AtomicUsize, Ordering, fence};

pub(super) struct ReaderEpoch {
    epoch: AtomicUsize,
    readers: [AtomicUsize; 2],
}

impl ReaderEpoch {
    pub(super) fn new() -> Self {
        Self {
            epoch: AtomicUsize::new(0),
            readers: [AtomicUsize::new(0), AtomicUsize::new(0)],
        }
    }

    /// Joins an epoch before the caller loads the published pointer.
    pub(super) fn enter(&self) -> Option<usize> {
        let epoch = self.epoch.load(Ordering::Acquire);
        // Admission and the epoch recheck share a total order with writer
        // closure and its zero check. Acquire/release alone permits both
        // sides to miss the other's publication and reclaim a raw reader.
        self.readers[epoch].fetch_add(1, Ordering::AcqRel);
        fence(Ordering::SeqCst);
        if self.epoch.load(Ordering::Acquire) != epoch {
            self.leave(epoch);
            return None;
        }
        Some(epoch)
    }

    /// Leaves only after the caller acquired an independent pointee owner.
    pub(super) fn leave(&self, epoch: usize) {
        self.readers[epoch].fetch_sub(1, Ordering::Release);
    }

    /// Closes the old epoch after publishing the replacement pointer.
    pub(super) fn advance(&self) -> usize {
        let previous = self.epoch.fetch_xor(1, Ordering::AcqRel);
        fence(Ordering::SeqCst);
        previous
    }

    pub(super) fn is_quiescent(&self, epoch: usize) -> bool {
        self.readers[epoch].load(Ordering::Acquire) == 0
    }
}

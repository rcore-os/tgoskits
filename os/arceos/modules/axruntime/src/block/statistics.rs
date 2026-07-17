//! Block completion counters owned by the runtime request lifecycle.

use core::sync::atomic::{AtomicU64, Ordering};

use rdif_block::RequestOp;

const LINUX_SECTOR_SIZE: u64 = 512;

/// A point-in-time snapshot of completed block I/O.
///
/// Sector counts use Linux's fixed 512-byte reporting unit regardless of the
/// device's logical block size.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BlockIoStats {
    reads_completed: u64,
    sectors_read: u64,
    writes_completed: u64,
    sectors_written: u64,
}

impl BlockIoStats {
    /// Returns successfully completed read requests.
    pub const fn reads_completed(self) -> u64 {
        self.reads_completed
    }

    /// Returns 512-byte sectors read by successful requests.
    pub const fn sectors_read(self) -> u64 {
        self.sectors_read
    }

    /// Returns successfully completed write requests.
    pub const fn writes_completed(self) -> u64 {
        self.writes_completed
    }

    /// Returns 512-byte sectors written by successful requests.
    pub const fn sectors_written(self) -> u64 {
        self.sectors_written
    }

    pub(super) fn saturating_add(self, other: Self) -> Self {
        Self {
            reads_completed: self.reads_completed.saturating_add(other.reads_completed),
            sectors_read: self.sectors_read.saturating_add(other.sectors_read),
            writes_completed: self.writes_completed.saturating_add(other.writes_completed),
            sectors_written: self.sectors_written.saturating_add(other.sectors_written),
        }
    }
}

pub(super) struct BlockIoCounters {
    reads_completed: AtomicU64,
    sectors_read: AtomicU64,
    writes_completed: AtomicU64,
    sectors_written: AtomicU64,
}

impl BlockIoCounters {
    pub(super) const fn new() -> Self {
        Self {
            reads_completed: AtomicU64::new(0),
            sectors_read: AtomicU64::new(0),
            writes_completed: AtomicU64::new(0),
            sectors_written: AtomicU64::new(0),
        }
    }

    pub(super) fn record_success(&self, operation: RequestOp, byte_len: usize) {
        let byte_len = u64::try_from(byte_len).unwrap_or(u64::MAX);
        let sectors = byte_len.div_ceil(LINUX_SECTOR_SIZE);
        match operation {
            RequestOp::Read => {
                saturating_increment(&self.reads_completed, 1);
                saturating_increment(&self.sectors_read, sectors);
            }
            RequestOp::Write => {
                saturating_increment(&self.writes_completed, 1);
                saturating_increment(&self.sectors_written, sectors);
            }
            RequestOp::Flush | RequestOp::Discard | RequestOp::WriteZeroes => {}
        }
    }

    pub(super) fn snapshot(&self) -> BlockIoStats {
        BlockIoStats {
            reads_completed: self.reads_completed.load(Ordering::Relaxed),
            sectors_read: self.sectors_read.load(Ordering::Relaxed),
            writes_completed: self.writes_completed.load(Ordering::Relaxed),
            sectors_written: self.sectors_written.load(Ordering::Relaxed),
        }
    }
}

fn saturating_increment(counter: &AtomicU64, amount: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(amount))
    });
}

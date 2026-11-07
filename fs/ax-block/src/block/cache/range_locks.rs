//! Exact range exclusion between direct requests and cache consumers.

use alloc::vec::Vec;

use crate::{
    BlockError, BlockResult,
    os::{sync::RawSpinLock, waiters::TaskWaiters},
};

#[derive(Clone, Copy, PartialEq, Eq)]
struct FrameRange {
    first: u64,
    last: u64,
}

impl FrameRange {
    fn overlaps(self, other: Self) -> bool {
        self.first <= other.last && other.first <= self.last
    }
}

pub(super) struct RangeLocks {
    active: RawSpinLock<Vec<FrameRange>>,
    waiters: TaskWaiters,
}

impl RangeLocks {
    pub(super) fn new() -> Self {
        Self {
            active: RawSpinLock::new(Vec::new()),
            waiters: TaskWaiters::new(),
        }
    }

    pub(super) fn lock(&self, first_frame: u64, last_frame: u64) -> BlockResult<RangeGuard<'_>> {
        if first_frame > last_frame {
            return Err(BlockError::InvalidRequest);
        }
        let requested = FrameRange {
            first: first_frame,
            last: last_frame,
        };
        let mut spare = Vec::new();
        loop {
            {
                let mut active = self.active.lock_irqsave();
                if !active.iter().any(|range| range.overlaps(requested)) {
                    if active.len() == active.capacity() {
                        let required = active.len().checked_add(1).ok_or(BlockError::NoMemory)?;
                        if spare.capacity() < required {
                            drop(active);
                            spare
                                .try_reserve_exact(required)
                                .map_err(|_| BlockError::NoMemory)?;
                            continue;
                        }
                        // Grow outside IRQ exclusion; move existing entries
                        // into the already-reserved buffer while locked.
                        spare.extend(active.drain(..));
                        core::mem::swap(&mut *active, &mut spare);
                    }
                    active.push(requested);
                    drop(active);
                    return Ok(RangeGuard {
                        ranges: self,
                        reserved: requested,
                    });
                }
            }
            self.waiters.wait_while(|| {
                self.active
                    .lock_irqsave()
                    .iter()
                    .any(|range| range.overlaps(requested))
            })?;
        }
    }
}

pub(super) struct RangeGuard<'a> {
    ranges: &'a RangeLocks,
    reserved: FrameRange,
}

impl Drop for RangeGuard<'_> {
    fn drop(&mut self) {
        let mut active = self.ranges.active.lock_irqsave();
        let position = active
            .iter()
            .position(|range| *range == self.reserved)
            .expect("active range reservation missing");
        active.swap_remove(position);
        drop(active);
        self.ranges.waiters.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reversed_range_does_not_reserve_stripes_or_wait() {
        let ranges = RangeLocks::new();
        assert!(matches!(ranges.lock(8, 7), Err(BlockError::InvalidRequest)));
        assert!(ranges.active.lock_irqsave().is_empty());
        assert_eq!(ranges.waiters.len(), 0);
    }

    #[test]
    fn a_wide_range_is_released_as_one_reservation() {
        let ranges = RangeLocks::new();
        let guard = ranges.lock(0, 128).unwrap();
        assert_eq!(ranges.active.lock_irqsave().len(), 1);
        drop(guard);
        assert!(ranges.active.lock_irqsave().is_empty());
        drop(ranges.lock(63, 64).unwrap());
    }

    #[test]
    fn wide_range_does_not_exclude_distant_single_frame() {
        use std::{
            sync::{Arc, mpsc},
            thread,
            time::Duration,
        };
        crate::os::task::install_test_runtime_ops();
        let ranges = Arc::new(RangeLocks::new());
        let wide = ranges.lock(0, 128).unwrap();
        let distant_ranges = Arc::clone(&ranges);
        let (done, received) = mpsc::channel();
        let task = thread::spawn(move || {
            let _distant = distant_ranges.lock(256, 256).unwrap();
            done.send(()).unwrap();
        });
        // Frame 256 hashes to frame 0's stripe, but does not overlap it.
        let advanced = received.recv_timeout(Duration::from_secs(5)).is_ok();
        drop(wide);
        task.join().unwrap();
        assert!(advanced, "distant frame waited for an unrelated wide range");
    }

    #[test]
    fn disjoint_reservations_progress_while_an_overlap_waits() {
        use std::{
            sync::{Arc, mpsc},
            thread,
            time::Duration,
        };
        crate::os::task::install_test_runtime_ops();
        let ranges = Arc::new(RangeLocks::new());
        let first = ranges.lock(3, 3).unwrap();
        let waiting = ranges.clone();
        let (done, received) = mpsc::channel();
        let task = thread::spawn(move || {
            let _overlap = waiting.lock(3, 4).unwrap();
            done.send(()).unwrap();
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while ranges.waiters.len() == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "overlap did not enter the wait path"
            );
            thread::yield_now();
        }
        assert!(matches!(
            received.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        let independent = ranges.lock(8, 8).unwrap();
        drop(independent);
        drop(first);
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        task.join().unwrap();
    }
}

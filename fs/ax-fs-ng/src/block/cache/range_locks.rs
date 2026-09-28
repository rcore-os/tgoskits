//! Bounded range exclusion between direct requests and cache consumers.

use crate::{
    BlockError, BlockResult,
    os::{sync::IrqMutex, waiters::TaskWaiters},
};

const STRIPES: usize = 64;

pub(super) struct RangeLocks {
    active: IrqMutex<u64>,
    waiters: TaskWaiters,
}

impl RangeLocks {
    pub(super) fn new() -> Self {
        Self {
            active: IrqMutex::new(0),
            waiters: TaskWaiters::new(),
        }
    }

    pub(super) fn lock(&self, first_frame: u64, last_frame: u64) -> BlockResult<RangeGuard<'_>> {
        let span = last_frame
            .checked_sub(first_frame)
            .ok_or(BlockError::InvalidRequest)?;
        let mask = if span >= (STRIPES - 1) as u64 {
            u64::MAX
        } else {
            (first_frame..=last_frame).fold(0, |mask, frame| mask | (1 << (frame % STRIPES as u64)))
        };
        loop {
            {
                let mut active = self.active.lock();
                if *active & mask == 0 {
                    *active |= mask;
                    return Ok(RangeGuard { ranges: self, mask });
                }
            }
            // Reserve every stripe atomically: no partial reservation or
            // nested OS locks survive while waiting for overlapping I/O.
            self.waiters
                .wait_while(|| *self.active.lock() & mask != 0)?;
        }
    }
}

pub(super) struct RangeGuard<'a> {
    ranges: &'a RangeLocks,
    mask: u64,
}

impl Drop for RangeGuard<'_> {
    fn drop(&mut self) {
        *self.ranges.active.lock() &= !self.mask;
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
        assert_eq!(*ranges.active.lock(), 0);
        assert_eq!(ranges.waiters.len(), 0);
    }

    #[test]
    fn a_wide_range_is_released_as_one_reservation() {
        let ranges = RangeLocks::new();
        let guard = ranges.lock(0, 128).unwrap();
        assert_eq!(*ranges.active.lock(), u64::MAX);
        drop(guard);
        assert_eq!(*ranges.active.lock(), 0);
        drop(ranges.lock(63, 64).unwrap());
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

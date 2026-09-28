//! In-flight range ownership and one-shot completion; no page bytes live here.

use alloc::{sync::Arc, vec::Vec};
use core::{
    ops::Range,
    sync::atomic::{AtomicBool, Ordering},
};

use axfs_ng_vfs::{VfsError, VfsResult};

use super::super::CachedPages;
use crate::os::{sync::SleepMutex, waiters::TaskWaiters};

// At most 16 windows of 32 pages: staging plus prepared pages use <= 4 MiB
// per inode. A full admission queue waits on an owner, never spins.
const MAX_PENDING_FILLS: usize = 16;

pub(in super::super) struct PendingFills {
    fills: SleepMutex<Vec<Arc<PendingFill>>>,
    capacity_waiters: TaskWaiters,
}

pub(super) enum FillAdmission<'a> {
    Wait(Arc<PendingFill>),
    Capacity,
    Load(FillOwner<'a>),
}

pub(super) struct PendingFill {
    range: Range<u64>,
    file_len: u64,
    valid: AtomicBool,
    complete: AtomicBool,
    outcome: SleepMutex<Option<VfsResult<()>>>,
    waiters: TaskWaiters,
}

pub(super) struct FillOwner<'a> {
    registry: &'a PendingFills,
    fill: Arc<PendingFill>,
    finished: bool,
}

impl PendingFills {
    pub(in super::super) const fn new() -> Self {
        Self {
            fills: SleepMutex::new(Vec::new()),
            capacity_waiters: TaskWaiters::new(),
        }
    }

    /// Admission and invalidation are serialized by the file's I/O owner.
    pub(super) fn admit(
        &self,
        first: u32,
        end: u64,
        file_len: u64,
        cache: &CachedPages,
    ) -> VfsResult<FillAdmission<'_>> {
        let mut fills = self.fills.lock();
        let first = u64::from(first);
        if let Some(fill) = fills
            .iter()
            .find(|fill| fill.valid.load(Ordering::Acquire) && fill.range.contains(&first))
        {
            return Ok(FillAdmission::Wait(Arc::clone(fill)));
        }
        if fills.len() >= MAX_PENDING_FILLS {
            return Ok(FillAdmission::Capacity);
        }
        let end = (first..end)
            .find(|number| {
                cache.contains(&(*number as u32))
                    || fills.iter().any(|fill| {
                        fill.valid.load(Ordering::Acquire) && fill.range.contains(number)
                    })
            })
            .unwrap_or(end);
        if end <= first {
            return Err(VfsError::BadState);
        }
        fills.try_reserve(1).map_err(|_| VfsError::NoMemory)?;
        let fill = Arc::new(PendingFill {
            range: first..end,
            file_len,
            valid: AtomicBool::new(true),
            complete: AtomicBool::new(false),
            outcome: SleepMutex::new(None),
            waiters: TaskWaiters::new(),
        });
        fills.push(Arc::clone(&fill));
        Ok(FillAdmission::Load(FillOwner {
            registry: self,
            fill,
            finished: false,
        }))
    }

    pub(in super::super) fn invalidate(&self) {
        for fill in self.fills.lock().iter() {
            // A ticket is never made valid again. No wrapping generation can
            // accidentally accept a read captured before a content mutation.
            fill.valid.store(false, Ordering::Release);
        }
    }

    pub(super) fn wait_for_capacity(&self) -> VfsResult<()> {
        self.capacity_waiters
            .wait_while(|| self.fills.lock().len() >= MAX_PENDING_FILLS)
            .map_err(crate::block_error_to_vfs_error)
    }

    #[cfg(test)]
    pub(in super::super) fn waiter_count(&self) -> usize {
        self.fills
            .lock()
            .iter()
            .map(|fill| fill.waiters.len())
            .sum()
    }

    #[cfg(test)]
    pub(in super::super) fn is_empty(&self) -> bool {
        self.fills.lock().is_empty()
    }
}

impl PendingFill {
    pub(super) fn wait(&self) -> VfsResult<()> {
        self.wait_ready()?;
        // Release publication of complete follows outcome initialization.
        self.outcome
            .lock()
            .as_ref()
            .copied()
            .ok_or(VfsError::BadState)?
    }

    pub(super) fn wait_ready(&self) -> VfsResult<()> {
        while !self.complete.load(Ordering::Acquire) {
            self.waiters
                .wait_while(|| !self.complete.load(Ordering::Acquire))
                .map_err(crate::block_error_to_vfs_error)?;
        }
        Ok(())
    }
}

impl FillOwner<'_> {
    pub(super) fn range(&self) -> Range<u64> {
        self.fill.range.clone()
    }
    pub(super) fn file_len(&self) -> u64 {
        self.fill.file_len
    }
    pub(super) fn is_valid(&self) -> bool {
        self.fill.valid.load(Ordering::Acquire)
    }

    pub(super) fn finish(mut self, result: VfsResult<()>) -> VfsResult<()> {
        self.publish(result);
        result
    }

    fn publish(&mut self, result: VfsResult<()>) {
        *self.fill.outcome.lock() = Some(result);
        self.fill.complete.store(true, Ordering::Release);
        self.registry
            .fills
            .lock()
            .retain(|fill| !Arc::ptr_eq(fill, &self.fill));
        self.finished = true;
        self.registry.capacity_waiters.notify_all();
        self.fill.waiters.notify_all();
    }
}

impl Drop for FillOwner<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.publish(Err(VfsError::Interrupted));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, thread, time::Duration};

    use super::*;

    #[test]
    fn ranges_coalesce_clip_and_invalidate_without_reusing_old_identity() {
        let registry = PendingFills::new();
        let cache = CachedPages::unbounded();
        let first = load(registry.admit(4, 8, 40960, &cache).unwrap());
        let FillAdmission::Wait(waiter) = registry.admit(6, 10, 40960, &cache).unwrap() else {
            panic!("overlapping demand did not join its owner");
        };
        assert!(Arc::ptr_eq(&waiter, &first.fill));
        let prefix = load(registry.admit(2, 7, 40960, &cache).unwrap());
        assert_eq!(prefix.range(), 2..4);
        registry.invalidate();
        assert!(!first.is_valid());
        assert!(!prefix.is_valid());
        let replacement = load(registry.admit(4, 8, 40960, &cache).unwrap());
        assert!(!Arc::ptr_eq(&replacement.fill, &first.fill));
        drop(first);
        assert_eq!(waiter.wait(), Err(VfsError::Interrupted));
        assert!(replacement.is_valid());
        replacement.finish(Ok(())).unwrap();
        drop(prefix);
        assert!(registry.is_empty());
    }

    #[test]
    fn admission_budget_wait_does_not_inherit_unrelated_read_failure() {
        let registry = PendingFills::new();
        let cache = CachedPages::unbounded();
        let mut owners = Vec::new();
        for number in 0..MAX_PENDING_FILLS as u32 {
            owners.push(load(
                registry
                    .admit(number, u64::from(number) + 1, 100000, &cache)
                    .unwrap(),
            ));
        }
        let FillAdmission::Capacity = registry.admit(100, 101, 500000, &cache).unwrap() else {
            panic!("admission exceeded its frame budget");
        };
        assert_eq!(registry.fills.lock().len(), MAX_PENDING_FILLS);
        assert_eq!(
            owners.remove(0).finish(Err(VfsError::Io)),
            Err(VfsError::Io)
        );
        crate::os::task::install_test_runtime_ops();
        assert_eq!(registry.wait_for_capacity(), Ok(()));
        let replacement = load(registry.admit(100, 101, 500000, &cache).unwrap());
        replacement.finish(Ok(())).unwrap();
        drop(owners);
        assert!(registry.is_empty());
    }

    #[test]
    fn admission_capacity_wakes_when_any_fill_finishes() {
        crate::os::task::install_test_runtime_ops();
        let registry = PendingFills::new();
        let cache = CachedPages::unbounded();
        let mut owners = Vec::new();
        for number in 0..MAX_PENDING_FILLS as u32 {
            owners.push(load(
                registry
                    .admit(number, u64::from(number) + 1, 100000, &cache)
                    .unwrap(),
            ));
        }
        assert!(matches!(
            registry.admit(100, 101, 500000, &cache).unwrap(),
            FillAdmission::Capacity
        ));

        thread::scope(|scope| {
            let (entered_tx, entered_rx) = mpsc::channel();
            let (done_tx, done_rx) = mpsc::channel();
            let waiting_registry = &registry;
            scope.spawn(move || {
                entered_tx.send(()).unwrap();
                waiting_registry.wait_for_capacity().unwrap();
                done_tx.send(()).unwrap();
            });
            entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            owners.remove(1).finish(Ok(())).unwrap();
            let woke_for_second = done_rx.recv_timeout(Duration::from_secs(2)).is_ok();
            owners.remove(0).finish(Ok(())).unwrap();
            assert!(woke_for_second, "capacity waiter stayed on the first fill");
        });
        drop(owners);
        assert!(registry.is_empty());
    }

    fn load(admission: FillAdmission<'_>) -> FillOwner<'_> {
        match admission {
            FillAdmission::Load(owner) => owner,
            _ => panic!("expected independent fill ownership"),
        }
    }
}

//! Cooperative synchronization primitives for RT tasks.

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::{
    executor::{RT_TASK_STATS, current_running_task, yield_current_task_with_state},
    state::RtTaskState,
};

/// A cooperative sleepable mutex for the isolated RT runtime.
pub struct RtMutex {
    owner: AtomicUsize,
    waiters: AtomicUsize,
}

impl RtMutex {
    /// Creates an unlocked RT mutex.
    pub const fn new() -> Self {
        Self {
            owner: AtomicUsize::new(usize::MAX),
            waiters: AtomicUsize::new(0),
        }
    }

    /// Locks the mutex, blocking the current RT task cooperatively if needed.
    pub fn lock(&self) -> RtMutexGuard<'_> {
        let task_id = current_running_task();
        loop {
            match self.owner.compare_exchange(
                usize::MAX,
                task_id,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return RtMutexGuard { mutex: self },
                Err(owner) if owner == task_id => {
                    panic!("RT mutex does not support recursive locking")
                }
                Err(_) => self.block_current_task(task_id),
            }
        }
    }

    fn block_current_task(&self, task_id: usize) {
        self.waiters.fetch_or(task_bit(task_id), Ordering::AcqRel);
        RT_TASK_STATS[task_id]
            .state
            .store(RtTaskState::Blocked as usize, Ordering::Release);
        yield_current_task_with_state(task_id);
    }

    fn unlock(&self) {
        let task_id = current_running_task();
        self.owner
            .compare_exchange(task_id, usize::MAX, Ordering::AcqRel, Ordering::Acquire)
            .expect("RT mutex unlock must be called by the owner task");
        self.wake_one_waiter();
    }

    fn wake_one_waiter(&self) {
        loop {
            let waiters = self.waiters.load(Ordering::Acquire);
            if waiters == 0 {
                return;
            }
            let task_id = waiters.trailing_zeros() as usize;
            let task_mask = task_bit(task_id);
            if self
                .waiters
                .compare_exchange(
                    waiters,
                    waiters & !task_mask,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                RT_TASK_STATS[task_id]
                    .state
                    .store(RtTaskState::Ready as usize, Ordering::Release);
                return;
            }
        }
    }
}

/// Guard returned by [`RtMutex::lock`].
pub struct RtMutexGuard<'mutex> {
    mutex: &'mutex RtMutex,
}

impl Drop for RtMutexGuard<'_> {
    fn drop(&mut self) {
        self.mutex.unlock();
    }
}

const fn task_bit(task_id: usize) -> usize {
    assert!(task_id < usize::BITS as usize);
    1usize << task_id
}

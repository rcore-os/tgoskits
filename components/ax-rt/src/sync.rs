//! Cooperative synchronization primitives for RT tasks.

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::{
    executor::{RT_TASK_STATS, current_running_task, yield_current_task_with_state},
    state::RtTaskState,
};

/// A cooperative sleepable mutex for the isolated RT runtime.
pub struct RtMutex {
    owner: AtomicUsize,
    recursion_depth: AtomicUsize,
    waiters: AtomicUsize,
}

impl RtMutex {
    /// Creates an unlocked RT mutex.
    pub const fn new() -> Self {
        Self {
            owner: AtomicUsize::new(usize::MAX),
            recursion_depth: AtomicUsize::new(0),
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
                Ok(_) => {
                    self.recursion_depth.store(1, Ordering::Release);
                    return RtMutexGuard { mutex: self };
                }
                Err(owner) if owner == task_id => return self.lock_recursive(task_id),
                Err(_) => self.block_current_task(task_id),
            }
        }
    }

    fn lock_recursive(&self, task_id: usize) -> RtMutexGuard<'_> {
        let _ = task_id;
        let depth = self.recursion_depth.load(Ordering::Acquire);
        assert!(depth > 0, "RT recursive mutex depth must be non-zero");
        self.recursion_depth.store(
            depth.checked_add(1).expect("RT mutex recursion overflow"),
            Ordering::Release,
        );
        RtMutexGuard { mutex: self }
    }

    fn block_current_task(&self, task_id: usize) {
        self.donate_priority_to_owner(task_id);
        self.waiters.fetch_or(task_bit(task_id), Ordering::AcqRel);
        RT_TASK_STATS[task_id]
            .state
            .store(RtTaskState::Blocked as usize, Ordering::Release);
        yield_current_task_with_state(task_id);
    }

    fn unlock(&self) {
        let task_id = current_running_task();
        assert_eq!(
            self.owner.load(Ordering::Acquire),
            task_id,
            "RT mutex unlock must be called by the owner task"
        );
        let depth = self.recursion_depth.load(Ordering::Acquire);
        assert!(depth > 0, "RT mutex recursion depth underflow");
        if depth > 1 {
            self.recursion_depth.store(depth - 1, Ordering::Release);
            return;
        }
        self.recursion_depth.store(0, Ordering::Release);
        self.owner
            .compare_exchange(task_id, usize::MAX, Ordering::AcqRel, Ordering::Acquire)
            .expect("RT mutex unlock must be called by the owner task");
        self.restore_owner_priority(task_id);
        self.wake_one_waiter();
    }

    fn donate_priority_to_owner(&self, waiter_task_id: usize) {
        let owner = self.owner.load(Ordering::Acquire);
        if owner == usize::MAX {
            return;
        }
        let waiter_priority = RT_TASK_STATS[waiter_task_id].effective_priority() as usize;
        let owner_priority = &RT_TASK_STATS[owner].effective_priority;
        let mut current = owner_priority.load(Ordering::Acquire);
        while waiter_priority > current {
            match owner_priority.compare_exchange(
                current,
                waiter_priority,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(updated) => current = updated,
            }
        }
    }

    fn restore_owner_priority(&self, owner_task_id: usize) {
        let base_priority = RT_TASK_STATS[owner_task_id]
            .base_priority
            .load(Ordering::Acquire);
        RT_TASK_STATS[owner_task_id]
            .effective_priority
            .store(base_priority, Ordering::Release);
    }

    fn wake_one_waiter(&self) {
        loop {
            let waiters = self.waiters.load(Ordering::Acquire);
            if waiters == 0 {
                return;
            }
            let task_id = highest_priority_waiter(waiters);
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

fn highest_priority_waiter(waiters: usize) -> usize {
    let mut selected = waiters.trailing_zeros() as usize;
    let mut selected_priority = RT_TASK_STATS[selected].effective_priority();
    let mut remaining = waiters & !task_bit(selected);
    while remaining != 0 {
        let task_id = remaining.trailing_zeros() as usize;
        let priority = RT_TASK_STATS[task_id].effective_priority();
        if priority > selected_priority {
            selected = task_id;
            selected_priority = priority;
        }
        remaining &= !task_bit(task_id);
    }
    selected
}

const fn task_bit(task_id: usize) -> usize {
    assert!(task_id < usize::BITS as usize);
    1usize << task_id
}

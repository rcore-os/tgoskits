use core::panic::Location;

use ax_lockdep::{self as common, HeldLockSnapshot, PreparedAcquire};
pub(crate) use ax_lockdep::{LockSubclass, LockdepMap};

use crate::mutex::RawMutex;

fn current_held_locks() -> HeldLockSnapshot {
    common::current_task_held_lock_snapshot()
}

pub(crate) struct LockdepAcquire {
    addr: usize,
    prepared: PreparedAcquire,
    inner: ax_lockdep::Lockdep,
}

impl LockdepAcquire {
    #[inline(always)]
    #[track_caller]
    pub(crate) fn prepare_nested(lock: &RawMutex, is_try: bool, subclass: LockSubclass) -> Self {
        let addr = lock as *const _ as *const () as usize;
        let prepared = common::prepare_acquire_with_snapshot_nested_with_sleep(
            &lock.lockdep,
            "mutex",
            addr,
            Location::caller(),
            current_held_locks(),
            subclass,
            false,
        );
        let inner = ax_lockdep::Lockdep::prepare("mutex", addr, is_try, None);
        Self {
            addr,
            prepared,
            inner,
        }
    }

    #[inline(always)]
    pub(crate) fn finish(self, acquired: bool) {
        self.inner.finish(acquired);
        if acquired {
            common::finish_acquire_task(self.prepared, self.addr);
        }
    }
}

#[inline(always)]
pub(crate) fn release(lock: &RawMutex) {
    let addr = lock as *const _ as *const () as usize;
    common::release_task(addr);
    ax_lockdep::Lockdep::release("mutex", addr, None);
}

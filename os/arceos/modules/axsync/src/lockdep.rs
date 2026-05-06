use core::panic::Location;

pub(crate) use ax_kspin::lockdep::LockdepMap;
use ax_kspin::lockdep::{self as common, HeldLockSnapshot, PreparedAcquire};

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
    pub(crate) fn prepare(lock: &RawMutex, is_try: bool) -> Self {
        let addr = lock as *const _ as *const () as usize;
        let prepared = common::prepare_acquire_with_snapshot(
            &lock.lockdep,
            "mutex",
            addr,
            Location::caller(),
            current_held_locks(),
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
    common::release_task(lock.lockdep.lock_id());
    let addr = lock as *const _ as *const () as usize;
    ax_lockdep::Lockdep::release("mutex", addr, None);
}

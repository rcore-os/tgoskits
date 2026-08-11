use core::panic::Location;

pub(crate) use crate::sync::spin::lockdep::LockSubclass;
use crate::sync::{
    mutex::RawMutex,
    spin::lockdep::{self as common, HeldLockSnapshot, PreparedAcquire},
};

fn current_held_locks() -> HeldLockSnapshot {
    common::current_task_held_lock_snapshot()
}

pub(crate) struct LockdepAcquire {
    addr: usize,
    prepared: PreparedAcquire,
    inner: crate::sync::lockdep::Lockdep,
}

impl LockdepAcquire {
    #[inline(always)]
    #[track_caller]
    pub(crate) fn prepare_nested(lock: &RawMutex, is_try: bool, subclass: LockSubclass) -> Self {
        let addr = lock as *const _ as *const () as usize;
        let prepared = crate::sync::lockdep::prepare_acquire_with_snapshot_nested_with_sleep(
            &lock.lockdep,
            "mutex",
            addr,
            Location::caller(),
            current_held_locks(),
            subclass,
            false,
        );
        let inner = crate::sync::lockdep::Lockdep::prepare("mutex", addr, is_try, None);
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
            crate::sync::lockdep::finish_acquire_task(self.prepared, self.addr);
        }
    }
}

#[inline(always)]
pub(crate) fn release(lock: &RawMutex) {
    let addr = lock as *const _ as *const () as usize;
    crate::sync::lockdep::release_task(addr);
    crate::sync::lockdep::Lockdep::release("mutex", addr, None);
}

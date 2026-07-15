use core::panic::Location;

use ax_lockdep::{self as common, HeldLockSnapshot, PreparedAcquire};
pub(crate) use ax_lockdep::{LockSubclass, LockdepMap};

use crate::mutex::RawMutex;

fn current_held_locks() -> HeldLockSnapshot {
    common::current_task_held_lock_snapshot()
}

pub(crate) struct LockdepAcquire<'lock> {
    addr: usize,
    lock: &'lock RawMutex,
    caller: &'static Location<'static>,
    subclass: LockSubclass,
    prepared: Option<PreparedAcquire>,
    inner: ax_lockdep::Lockdep,
}

impl<'lock> LockdepAcquire<'lock> {
    #[inline(always)]
    #[track_caller]
    pub(crate) fn prepare_nested(
        lock: &'lock RawMutex,
        is_try: bool,
        subclass: LockSubclass,
    ) -> Self {
        let addr = lock as *const _ as *const () as usize;
        let caller = Location::caller();
        // A failed try-lock cannot block or add a dependency edge. Defer its
        // lock-order validation until the raw acquisition succeeds, matching
        // the acquisition ordering used by non-sleeping locks.
        let prepared = (!is_try).then(|| {
            common::prepare_acquire_with_snapshot_nested_with_sleep(
                &lock.lockdep,
                "mutex",
                addr,
                caller,
                current_held_locks(),
                subclass,
                false,
            )
        });
        let inner = ax_lockdep::Lockdep::prepare("mutex", addr, is_try, None);
        Self {
            addr,
            lock,
            caller,
            subclass,
            prepared,
            inner,
        }
    }

    #[inline(always)]
    pub(crate) fn finish(self, acquired: bool) {
        self.inner.finish(acquired);
        if acquired {
            let prepared = self.prepared.unwrap_or_else(|| {
                common::prepare_acquire_with_snapshot_nested_with_sleep(
                    &self.lock.lockdep,
                    "mutex",
                    self.addr,
                    self.caller,
                    current_held_locks(),
                    self.subclass,
                    false,
                )
            });
            common::finish_acquire_task(prepared, self.addr);
        }
    }
}

#[inline(always)]
pub(crate) fn release(lock: &RawMutex) {
    let addr = lock as *const _ as *const () as usize;
    common::release_task(addr);
    ax_lockdep::Lockdep::release("mutex", addr, None);
}

use core::panic::Location;

use ax_kspin::lockdep::{self as common, HeldLockSnapshot, PreparedAcquire};

use crate::mutex::RawMutex;

pub(crate) use ax_kspin::lockdep::LockdepMap;

fn current_held_locks() -> HeldLockSnapshot {
    let mut snapshot = common::current_cpu_held_lock_snapshot();
    ax_task::with_current_lockdep_stack(|stack| snapshot.extend(stack));
    snapshot
}

pub(crate) struct LockdepAcquire {
    addr: usize,
    prepared: PreparedAcquire,
}

impl LockdepAcquire {
    #[inline(always)]
    #[track_caller]
    pub(crate) fn prepare(lock: &RawMutex) -> Self {
        let addr = lock as *const _ as *const () as usize;
        let prepared = common::prepare_acquire_with_snapshot(
            &lock.lockdep,
            "mutex",
            addr,
            Location::caller(),
            current_held_locks(),
        );
        Self { addr, prepared }
    }

    #[inline(always)]
    pub(crate) fn finish(self) {
        ax_task::with_current_lockdep_stack(|stack| {
            common::finish_acquire_with_stack(self.prepared, self.addr, stack);
        });
    }
}

#[inline(always)]
pub(crate) fn release(lock: &RawMutex) {
    ax_task::with_current_lockdep_stack(|stack| {
        common::release_from_stack(lock.lockdep.lock_id(), stack);
    });
}

use core::panic::Location;

use crate::sync::{
    lockdep::{self as common, HeldLockSnapshot, LockSubclass, LockdepMapView, PreparedAcquire},
    mutex::RawMutex,
};

fn current_held_locks() -> HeldLockSnapshot {
    common::current_task_held_lock_snapshot()
}

pub(crate) struct LockdepAcquire {
    addr: usize,
    prepared: PreparedAcquire,
    inner: common::Lockdep,
}

pub(in crate::sync) struct LockdepAcquireRequest<'lock> {
    pub map: LockdepMapView<'lock>,
    pub addr: usize,
    pub subclass: LockSubclass,
    pub is_try: bool,
    pub caller: &'static Location<'static>,
}

impl LockdepAcquire {
    #[inline(always)]
    #[track_caller]
    pub(crate) fn prepare_nested(lock: &RawMutex, is_try: bool, subclass: LockSubclass) -> Self {
        let addr = lock as *const _ as *const () as usize;
        Self::prepare_view(LockdepAcquireRequest {
            map: lock.lockdep.view(),
            addr,
            subclass,
            is_try,
            caller: Location::caller(),
        })
    }

    pub(in crate::sync) fn prepare_view(request: LockdepAcquireRequest<'_>) -> Self {
        let prepared = common::prepare_acquire_with_snapshot_view_nested_with_sleep(
            request.map,
            "mutex",
            request.addr,
            request.caller,
            current_held_locks(),
            request.subclass,
            false,
        );
        let inner = common::Lockdep::prepare("mutex", request.addr, request.is_try, None);
        Self {
            addr: request.addr,
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
    release_external(addr);
}

#[inline(always)]
pub(in crate::sync) fn release_external(addr: usize) {
    common::release_task(addr);
    common::Lockdep::release("mutex", addr, None);
}

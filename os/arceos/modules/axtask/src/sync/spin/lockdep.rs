use core::{any::type_name, panic::Location};

use super::base::BaseSpinLock;
use crate::sync::context::GuardState;
pub use crate::sync::lockdep::*;

#[derive(Clone, Copy)]
pub(crate) struct Lockdep {
    addr: usize,
    inner: crate::sync::lockdep::Lockdep,
    prepared: Option<crate::sync::lockdep::PreparedAcquire>,
}

impl Lockdep {
    #[inline(always)]
    #[track_caller]
    pub(crate) fn prepare<G: GuardState, T: ?Sized>(
        lock: &BaseSpinLock<G, T>,
        is_try: bool,
    ) -> Self {
        Self::prepare_nested(lock, is_try, crate::sync::lockdep::DEFAULT_LOCK_SUBCLASS)
    }

    #[inline(always)]
    #[track_caller]
    pub(crate) fn prepare_nested<G: GuardState, T: ?Sized>(
        lock: &BaseSpinLock<G, T>,
        is_try: bool,
        subclass: crate::sync::lockdep::LockSubclass,
    ) -> Self {
        let addr = lock as *const _ as *const () as usize;
        Self::prepare_map::<G>(
            lock.lockdep_map(),
            "spin lock",
            "spin",
            addr,
            is_try,
            subclass,
            Some(HeldLockMode::Exclusive),
        )
    }

    #[inline(always)]
    #[track_caller]
    pub(crate) fn prepare_map<G: GuardState>(
        map: &crate::sync::lockdep::LockdepMap,
        lock_kind: &'static str,
        trace_kind: &'static str,
        addr: usize,
        is_try: bool,
        subclass: crate::sync::lockdep::LockSubclass,
        task_mode: Option<HeldLockMode>,
    ) -> Self {
        let prepared = task_mode
            .filter(|mode| tracks_task_locks::<G>(*mode))
            .map(|mode| {
                crate::sync::lockdep::prepare_acquire_with_snapshot_nested_mode(
                    map,
                    lock_kind,
                    addr,
                    Location::caller(),
                    crate::sync::lockdep::current_task_held_lock_snapshot(),
                    subclass,
                    mode,
                )
            });
        Self {
            addr,
            inner: crate::sync::lockdep::Lockdep::prepare(
                trace_kind,
                addr,
                is_try,
                Some(core::any::type_name::<G>()),
            ),
            prepared,
        }
    }

    #[inline(always)]
    pub(crate) fn finish(&self, acquired: bool) {
        self.inner.finish(acquired);
        if let (true, Some(prepared)) = (acquired, self.prepared) {
            crate::sync::lockdep::finish_acquire_task(prepared, self.addr);
        }
    }

    #[inline(always)]
    pub(crate) fn lock_addr(&self) -> usize {
        self.addr
    }
}

#[inline(always)]
pub(crate) fn release<G: GuardState>(addr: usize) {
    release_kind::<G>("spin", addr);
}

#[inline(always)]
pub(crate) fn release_kind<G: GuardState>(kind: &'static str, addr: usize) {
    release_kind_mode::<G>(kind, addr, HeldLockMode::Exclusive);
}

#[inline(always)]
pub(crate) fn release_kind_mode<G: GuardState>(
    kind: &'static str,
    addr: usize,
    mode: HeldLockMode,
) {
    if tracks_task_locks::<G>(mode) {
        crate::sync::lockdep::release_task(addr);
    }
    crate::sync::lockdep::Lockdep::release(kind, addr, Some(core::any::type_name::<G>()));
}

#[inline(always)]
pub(crate) fn release_trace_only<G: GuardState>(kind: &'static str, addr: usize) {
    crate::sync::lockdep::Lockdep::release(kind, addr, Some(core::any::type_name::<G>()));
}

#[inline(always)]
pub(crate) fn force_release<G: GuardState>(addr: usize) {
    if tracks_task_locks::<G>(HeldLockMode::Exclusive) {
        crate::sync::lockdep::force_release_task(addr);
    }
    crate::sync::lockdep::Lockdep::release("spin", addr, Some(core::any::type_name::<G>()));
}

fn is_noop_guard<G: GuardState>() -> bool {
    type_name::<G>() == type_name::<crate::sync::context::RawState>()
}

fn tracks_task_locks<G: GuardState>(mode: HeldLockMode) -> bool {
    if mode == HeldLockMode::Read && is_noop_guard::<G>() {
        return false;
    }
    is_noop_guard::<G>() || G::lockdep_enabled()
}

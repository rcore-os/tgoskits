use core::{any::type_name, panic::Location};

pub use crate::lockdep_core::{
    DEFAULT_LOCK_SUBCLASS, HeldLock, HeldLockKind, HeldLockSnapshot, HeldLockStack, LockSubclass,
    LockdepMap, LockdepOps, PreparedAcquire, current_task_held_lock_snapshot,
};
use crate::{context::GuardState, spin_base::BaseSpinLock};

/// Enables or disables lockdep trace recording.
pub fn set_lockdep_trace_enabled(enabled: bool) {
    crate::lockdep_core::set_trace_enabled(enabled);
}

/// Dumps the current lockdep trace buffer through the lockdep runtime console.
pub fn dump_lockdep_trace() {
    crate::lockdep_core::dump_trace_buffer();
}

#[derive(Clone, Copy)]
pub(crate) struct Lockdep {
    addr: usize,
    inner: crate::lockdep_core::Lockdep,
    prepared: Option<crate::lockdep_core::PreparedAcquire>,
}

impl Lockdep {
    #[inline(always)]
    #[track_caller]
    pub(crate) fn prepare<G: GuardState, T: ?Sized>(
        lock: &BaseSpinLock<G, T>,
        is_try: bool,
    ) -> Self {
        Self::prepare_nested(lock, is_try, crate::lockdep_core::DEFAULT_LOCK_SUBCLASS)
    }

    #[inline(always)]
    #[track_caller]
    pub(crate) fn prepare_nested<G: GuardState, T: ?Sized>(
        lock: &BaseSpinLock<G, T>,
        is_try: bool,
        subclass: crate::lockdep_core::LockSubclass,
    ) -> Self {
        let addr = lock as *const _ as *const () as usize;
        Self::prepare_map::<G>(
            lock.lockdep_map(),
            "spin lock",
            "spin",
            addr,
            is_try,
            subclass,
            true,
        )
    }

    #[inline(always)]
    #[track_caller]
    pub(crate) fn prepare_map<G: GuardState>(
        map: &crate::lockdep_core::LockdepMap,
        lock_kind: &'static str,
        trace_kind: &'static str,
        addr: usize,
        is_try: bool,
        subclass: crate::lockdep_core::LockSubclass,
        track_task_lock: bool,
    ) -> Self {
        let prepared = if track_task_lock && tracks_task_locks::<G>() {
            Some(crate::lockdep_core::prepare_acquire_with_snapshot_nested(
                map,
                lock_kind,
                addr,
                Location::caller(),
                crate::lockdep_core::current_task_held_lock_snapshot(),
                subclass,
            ))
        } else {
            None
        };
        Self {
            addr,
            inner: crate::lockdep_core::Lockdep::prepare(
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
            crate::lockdep_core::finish_acquire_task(prepared, self.addr);
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
    if tracks_task_locks::<G>() {
        crate::lockdep_core::release_task(addr);
    }
    crate::lockdep_core::Lockdep::release(kind, addr, Some(core::any::type_name::<G>()));
}

#[inline(always)]
pub(crate) fn release_trace_only<G: GuardState>(kind: &'static str, addr: usize) {
    crate::lockdep_core::Lockdep::release(kind, addr, Some(core::any::type_name::<G>()));
}

#[inline(always)]
pub(crate) fn force_release<G: GuardState>(addr: usize) {
    if tracks_task_locks::<G>() {
        crate::lockdep_core::force_release_task(addr);
    }
    crate::lockdep_core::Lockdep::release("spin", addr, Some(core::any::type_name::<G>()));
}

fn is_noop_guard<G: GuardState>() -> bool {
    type_name::<G>() == type_name::<crate::context::RawState>()
}

fn tracks_task_locks<G: GuardState>() -> bool {
    is_noop_guard::<G>() || G::lockdep_enabled()
}

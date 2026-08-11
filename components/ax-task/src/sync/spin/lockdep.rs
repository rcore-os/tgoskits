use core::{any::type_name, panic::Location};

pub use ax_sync::{
    DEFAULT_LOCK_SUBCLASS, LockdepMap, PreparedAcquire, current_task_held_lock_snapshot,
};

use super::base::BaseSpinLock;
use crate::sync::context::GuardState;

#[derive(Clone, Copy)]
pub(crate) struct Lockdep {
    addr: usize,
    inner: ax_sync::ExternalLockdepTrace,
    prepared: Option<PreparedAcquire>,
}

impl Lockdep {
    #[inline(always)]
    #[track_caller]
    pub(crate) fn prepare<G: GuardState, T: ?Sized>(
        lock: &BaseSpinLock<G, T>,
        is_try: bool,
    ) -> Self {
        Self::prepare_nested(lock, is_try, DEFAULT_LOCK_SUBCLASS)
    }

    #[inline(always)]
    #[track_caller]
    pub(crate) fn prepare_nested<G: GuardState, T: ?Sized>(
        lock: &BaseSpinLock<G, T>,
        is_try: bool,
        subclass: u32,
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
        map: &LockdepMap,
        lock_kind: &'static str,
        trace_kind: &'static str,
        addr: usize,
        is_try: bool,
        subclass: u32,
        track_task_lock: bool,
    ) -> Self {
        let prepared = if track_task_lock && tracks_task_locks::<G>() {
            Some(ax_sync::prepare_acquire_with_snapshot_nested(
                map,
                lock_kind,
                addr,
                Location::caller(),
                current_task_held_lock_snapshot(),
                subclass,
            ))
        } else {
            None
        };
        Self {
            addr,
            inner: ax_sync::ExternalLockdepTrace::prepare(
                trace_kind,
                addr,
                is_try,
                Some(type_name::<G>()),
            ),
            prepared,
        }
    }

    #[inline(always)]
    pub(crate) fn finish(&self, acquired: bool) {
        self.inner.finish(acquired);
        if let (true, Some(prepared)) = (acquired, self.prepared) {
            ax_sync::finish_acquire_task(prepared, self.addr);
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
        ax_sync::release_task(addr);
    }
    ax_sync::ExternalLockdepTrace::release(kind, addr, Some(type_name::<G>()));
}

#[inline(always)]
pub(crate) fn release_trace_only<G: GuardState>(kind: &'static str, addr: usize) {
    ax_sync::ExternalLockdepTrace::release(kind, addr, Some(type_name::<G>()));
}

#[inline(always)]
pub(crate) fn force_release<G: GuardState>(addr: usize) {
    if tracks_task_locks::<G>() {
        ax_sync::force_release_task(addr);
    }
    ax_sync::ExternalLockdepTrace::release("spin", addr, Some(type_name::<G>()));
}

fn is_noop_guard<G: GuardState>() -> bool {
    type_name::<G>() == type_name::<crate::sync::context::RawState>()
}

fn tracks_task_locks<G: GuardState>() -> bool {
    is_noop_guard::<G>() || G::lockdep_enabled()
}

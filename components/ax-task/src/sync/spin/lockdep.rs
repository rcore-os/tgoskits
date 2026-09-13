use core::{any::type_name, panic::Location};

use super::base::BaseSpinLock;
pub use crate::sync::lockdep::{
    DEFAULT_LOCK_SUBCLASS, LockdepMap, PreparedAcquire, current_task_held_lock_snapshot,
};
use crate::sync::{
    context::GuardState,
    lockdep::{LockdepMapView, prepare_acquire_with_snapshot_view_nested_with_sleep},
};

#[derive(Clone, Copy)]
pub(crate) struct Lockdep {
    addr: usize,
    inner: crate::sync::lockdep::Lockdep,
    prepared: Option<PreparedAcquire>,
}

pub(crate) struct LockdepAcquireRequest<'map> {
    pub map: LockdepMapView<'map>,
    pub lock_kind: &'static str,
    pub trace_kind: &'static str,
    pub addr: usize,
    pub is_try: bool,
    pub subclass: u32,
    pub caller: &'static Location<'static>,
    pub detail: &'static str,
    pub track_task_lock: bool,
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
        Self::prepare_view(LockdepAcquireRequest {
            map: map.view(),
            lock_kind,
            trace_kind,
            addr,
            is_try,
            subclass,
            caller: Location::caller(),
            detail: type_name::<G>(),
            track_task_lock: track_task_lock && tracks_task_locks::<G>(),
        })
    }

    #[inline(always)]
    pub(crate) fn prepare_view(request: LockdepAcquireRequest<'_>) -> Self {
        let prepared = request.track_task_lock.then(|| {
            prepare_acquire_with_snapshot_view_nested_with_sleep(
                request.map,
                request.lock_kind,
                request.addr,
                request.caller,
                current_task_held_lock_snapshot(),
                request.subclass,
                true,
            )
        });
        Self {
            addr: request.addr,
            inner: crate::sync::lockdep::Lockdep::prepare(
                request.trace_kind,
                request.addr,
                request.is_try,
                Some(request.detail),
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
    release_external(kind, addr, type_name::<G>(), tracks_task_locks::<G>());
}

#[inline(always)]
pub(crate) fn release_trace_only<G: GuardState>(kind: &'static str, addr: usize) {
    release_external(kind, addr, type_name::<G>(), false);
}

#[inline(always)]
pub(crate) fn force_release<G: GuardState>(addr: usize) {
    force_release_external("spin", addr, type_name::<G>(), tracks_task_locks::<G>());
}

#[inline(always)]
pub(crate) fn release_external(
    kind: &'static str,
    addr: usize,
    detail: &'static str,
    track_task_lock: bool,
) {
    if track_task_lock {
        crate::sync::lockdep::release_task(addr);
    }
    crate::sync::lockdep::Lockdep::release(kind, addr, Some(detail));
}

#[inline(always)]
pub(crate) fn force_release_external(
    kind: &'static str,
    addr: usize,
    detail: &'static str,
    track_task_lock: bool,
) {
    if track_task_lock {
        crate::sync::lockdep::force_release_task(addr);
    }
    crate::sync::lockdep::Lockdep::release(kind, addr, Some(detail));
}

fn is_noop_guard<G: GuardState>() -> bool {
    type_name::<G>() == type_name::<crate::sync::context::RawState>()
}

fn tracks_task_locks<G: GuardState>() -> bool {
    is_noop_guard::<G>() || G::lockdep_enabled()
}

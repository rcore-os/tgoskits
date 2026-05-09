use core::{any::type_name, panic::Location};

use ax_kernel_guard::BaseGuard;
pub use ax_lockdep::{
    HeldLock, HeldLockSnapshot, HeldLockStack, KspinLockdepIf, LockdepMap, PreparedAcquire,
    current_task_held_lock_snapshot, finish_acquire_task, finish_acquire_with_stack,
    force_release_task, prepare_acquire_with_snapshot, release_from_stack, release_task,
};

use crate::base::BaseSpinLock;

#[derive(Clone, Copy)]
pub(crate) struct Lockdep {
    addr: usize,
    lock_id: Option<u32>,
    inner: ax_lockdep::Lockdep,
    prepared: Option<ax_lockdep::PreparedAcquire>,
}

impl Lockdep {
    #[inline(always)]
    #[track_caller]
    pub(crate) fn prepare<G: BaseGuard, T: ?Sized>(
        lock: &BaseSpinLock<G, T>,
        is_try: bool,
    ) -> Self {
        let addr = lock as *const _ as *const () as usize;
        let prepared = if tracks_task_locks::<G>() {
            Some(ax_lockdep::prepare_acquire_with_snapshot(
                lock.lockdep_map(),
                "spin lock",
                addr,
                Location::caller(),
                ax_lockdep::current_task_held_lock_snapshot(),
            ))
        } else {
            None
        };
        Self {
            addr,
            lock_id: prepared.map(ax_lockdep::PreparedAcquire::lock_id),
            inner: ax_lockdep::Lockdep::prepare(
                "spin",
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
            ax_lockdep::finish_acquire_task(prepared, self.addr);
        }
    }

    #[inline(always)]
    pub(crate) fn lock_addr(&self) -> usize {
        self.addr
    }

    #[inline(always)]
    pub(crate) fn lock_id(&self) -> Option<u32> {
        self.lock_id
    }
}

#[inline(always)]
pub(crate) fn release<G: BaseGuard>(lock_id: Option<u32>, addr: usize) {
    if tracks_task_locks::<G>() {
        ax_lockdep::release_task(lock_id);
    }
    ax_lockdep::Lockdep::release("spin", addr, Some(core::any::type_name::<G>()));
}

#[inline(always)]
pub(crate) fn force_release<G: BaseGuard>(map: &LockdepMap, addr: usize) {
    if tracks_task_locks::<G>() {
        ax_lockdep::force_release_task(map);
    }
    ax_lockdep::Lockdep::release("spin", addr, Some(core::any::type_name::<G>()));
}

fn is_noop_guard<G: BaseGuard>() -> bool {
    type_name::<G>() == type_name::<ax_kernel_guard::NoOp>()
}

fn tracks_task_locks<G: BaseGuard>() -> bool {
    is_noop_guard::<G>() || G::lockdep_enabled()
}

use core::panic::Location;

use ax_lockdep::{self, LockdepMap, PreparedAcquire};

#[derive(Clone, Copy)]
pub(crate) struct LockdepAcquire {
    addr: usize,
    prepared: PreparedAcquire,
    inner: ax_lockdep::Lockdep,
}

impl LockdepAcquire {
    #[inline(always)]
    #[track_caller]
    pub(crate) fn prepare<T: ?Sized, R>(lock: &crate::mutex::Mutex<T, R>, is_try: bool) -> Self {
        let addr = lock as *const _ as *const () as usize;
        let prepared = ax_lockdep::prepare_acquire_with_snapshot(
            lock.lockdep_map(),
            "spin::Mutex",
            addr,
            Location::caller(),
            ax_lockdep::current_task_held_lock_snapshot(),
        );
        let inner = ax_lockdep::Lockdep::prepare("spin::Mutex", addr, is_try, None);
        Self {
            addr,
            prepared,
            inner,
        }
    }

    #[inline(always)]
    pub(crate) fn finish(&self, acquired: bool) {
        self.inner.finish(acquired);
        if acquired {
            ax_lockdep::finish_acquire_task(self.prepared, self.addr);
        }
    }

    #[inline(always)]
    pub(crate) fn lock_addr(&self) -> usize {
        self.addr
    }
}

#[inline(always)]
pub(crate) fn release(addr: usize) {
    ax_lockdep::release_task(addr);
    ax_lockdep::Lockdep::release("spin::Mutex", addr, None);
}

#[inline(always)]
pub(crate) fn force_release(addr: usize) {
    ax_lockdep::force_release_task(addr);
    ax_lockdep::Lockdep::release("spin::Mutex", addr, None);
}

pub(crate) type Map = LockdepMap;

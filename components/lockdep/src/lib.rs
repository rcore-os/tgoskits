#![cfg_attr(all(not(test), target_os = "none"), no_std)]

#[cfg(any(test, doctest, not(target_os = "none")))]
extern crate std;

mod state;
mod trace;

pub use self::{
    state::{
        HeldLock, HeldLockSnapshot, HeldLockStack, KspinLockdepIf, LockdepCheckError, LockdepMap,
        PreparedAcquire, current_task_held_lock_snapshot, finish_acquire_task,
        finish_acquire_with_stack, force_release_task, prepare_acquire_with_snapshot,
        prepare_acquire_with_snapshot_checked, release_from_stack, release_task,
    },
    trace::{dump_trace_buffer, set_trace_enabled},
};

#[derive(Clone, Copy)]
pub struct Lockdep {
    addr: usize,
    is_try: bool,
    kind: &'static str,
    detail: Option<&'static str>,
}

impl Lockdep {
    #[inline(always)]
    pub fn prepare(
        kind: &'static str,
        addr: usize,
        is_try: bool,
        detail: Option<&'static str>,
    ) -> Self {
        trace::trace_lock_begin(kind, addr, is_try, detail);
        Self {
            addr,
            is_try,
            kind,
            detail,
        }
    }

    #[inline(always)]
    pub fn finish(&self, acquired: bool) {
        trace::trace_lock_finish(self.kind, self.addr, self.is_try, acquired, self.detail);
    }

    #[inline(always)]
    pub fn lock_addr(&self) -> usize {
        self.addr
    }

    #[inline(always)]
    pub fn release(kind: &'static str, addr: usize, detail: Option<&'static str>) {
        trace::trace_unlock(kind, addr, detail);
    }
}

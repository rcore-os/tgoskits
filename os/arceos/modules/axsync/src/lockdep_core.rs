#[path = "lockdep_state.rs"]
mod state;
#[path = "lockdep_trace.rs"]
mod trace;

#[cfg(feature = "sleep")]
pub use self::state::prepare_acquire_with_snapshot_nested_with_sleep;
pub use self::{
    state::{
        DEFAULT_LOCK_SUBCLASS, HeldLock, HeldLockKind, HeldLockSnapshot, HeldLockStack,
        LockSubclass, LockdepMap, LockdepOps, PreparedAcquire, current_task_held_lock_snapshot,
        finish_acquire_task, force_release_task, prepare_acquire_with_snapshot_nested,
        release_task,
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
    pub fn release(kind: &'static str, addr: usize, detail: Option<&'static str>) {
        trace::trace_unlock(kind, addr, detail);
    }
}

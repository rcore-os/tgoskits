mod state;
mod trace;

#[cfg(test)]
pub use self::state::HeldLockKind;
pub use self::{
    state::{
        DEFAULT_LOCK_SUBCLASS, HeldLock, HeldLockSnapshot, HeldLockStack, LockSubclass, LockdepMap,
        PreparedAcquire, current_task_held_lock_snapshot, finish_acquire_task, force_release_task,
        prepare_acquire_with_snapshot_nested, release_task,
    },
    trace::{dump_trace_buffer, set_trace_enabled},
};

/// Enables or disables lockdep trace recording.
pub fn set_lockdep_trace_enabled(enabled: bool) {
    set_trace_enabled(enabled);
}

/// Dumps the current lockdep trace buffer through the emergency console.
pub fn dump_lockdep_trace() {
    dump_trace_buffer();
}

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

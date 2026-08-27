//! Lock dependency graph, diagnostics, and runtime capability boundary.

mod backend;
pub(crate) mod mutex;
mod state;
mod trace;
mod types;

pub(crate) use self::trace::*;
pub use self::{state::*, types::*};

#[cfg(all(test, feature = "lockdep", feature = "smp"))]
pub(crate) const TEST_MAX_HELD_LOCKS: usize = types::MAX_HELD_LOCKS;

/// Enables or disables lockdep trace recording.
pub fn set_lockdep_trace_enabled(enabled: bool) {
    set_trace_enabled(enabled);
}

/// Dumps the current lockdep trace buffer through the lockdep runtime console.
pub fn dump_lockdep_trace() {
    dump_trace_buffer();
}

#[derive(Clone, Copy)]
pub(crate) struct Lockdep {
    addr: usize,
    is_try: bool,
    kind: &'static str,
    detail: Option<&'static str>,
}

impl Lockdep {
    #[inline(always)]
    pub(crate) fn prepare(
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
    pub(crate) fn finish(&self, acquired: bool) {
        trace::trace_lock_finish(self.kind, self.addr, self.is_try, acquired, self.detail);
    }

    #[inline(always)]
    pub(crate) fn release(kind: &'static str, addr: usize, detail: Option<&'static str>) {
        trace::trace_unlock(kind, addr, detail);
    }
}

//! Stable deterministic snapshot of one owner CPU.

use super::CpuLocal;
use crate::{CpuId, ThreadId};

/// Stable, allocation-free scheduler state used by deterministic model tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CpuSnapshot {
    owner: CpuId,
    current: Option<ThreadId>,
    runnable: usize,
    need_resched: bool,
}

impl CpuSnapshot {
    pub(crate) fn capture(cpu: &CpuLocal) -> Self {
        Self {
            owner: cpu.owner,
            current: cpu.current,
            runnable: cpu.runnable_count(),
            need_resched: cpu.needs_reschedule(),
        }
    }

    /// Returns the owner CPU.
    pub const fn owner(self) -> CpuId {
        self.owner
    }

    /// Returns the current thread.
    pub const fn current(self) -> Option<ThreadId> {
        self.current
    }

    /// Returns the number of runnable threads.
    pub const fn runnable(self) -> usize {
        self.runnable
    }

    /// Returns the sticky preemption state.
    pub const fn need_resched(self) -> bool {
        self.need_resched
    }
}

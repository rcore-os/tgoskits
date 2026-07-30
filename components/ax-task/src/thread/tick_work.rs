//! Scheduler-tick-gated extension work executed in ordinary task context.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use super::ThreadId;

/// Shared process or subsystem interest in scheduler-tick task work.
///
/// An operating system may share one gate across every scheduler thread that
/// belongs to the same higher-level accounting domain. The hard-IRQ path only
/// observes this atomic gate; it never invokes the associated callback.
#[derive(Debug)]
pub struct SchedulerTickGate {
    state: AtomicU64,
}

impl SchedulerTickGate {
    const ENABLED: u64 = 1;
    const GENERATION_STEP: u64 = 2;

    /// Creates a disabled scheduler-tick gate.
    pub const fn new() -> Self {
        Self {
            state: AtomicU64::new(0),
        }
    }

    /// Publishes whether scheduler ticks should enqueue deferred extension work.
    ///
    /// A disable transition invalidates every queued publication from the
    /// previous enabled generation. It does not wait for a callback that an
    /// ordinary-context consumer already claimed.
    pub fn set_enabled(&self, enabled: bool) {
        let mut observed = self.state.load(Ordering::Acquire);
        loop {
            if (observed & Self::ENABLED != 0) == enabled {
                return;
            }
            let generation = observed
                .checked_add(Self::GENERATION_STEP)
                .expect("scheduler tick gate generation overflow");
            let updated = (generation & !Self::ENABLED) | u64::from(enabled);
            match self.state.compare_exchange_weak(
                observed,
                updated,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(current) => observed = current,
            }
        }
    }

    fn enabled_generation(&self) -> Option<u64> {
        let state = self.state.load(Ordering::Acquire);
        (state & Self::ENABLED != 0).then_some(state)
    }

    fn generation_is_enabled(&self, generation: u64) -> bool {
        self.state.load(Ordering::Acquire) == generation
    }
}

impl Default for SchedulerTickGate {
    fn default() -> Self {
        Self::new()
    }
}

/// Task-context callback selected by one scheduler-tick publication.
pub type SchedulerTickTaskWork = unsafe extern "Rust" fn(data: usize, thread: ThreadId);

#[derive(Clone, Debug)]
pub(crate) struct SchedulerTickWork {
    gate: Arc<SchedulerTickGate>,
    callback: SchedulerTickTaskWork,
}

impl SchedulerTickWork {
    pub(crate) const fn new(gate: Arc<SchedulerTickGate>, callback: SchedulerTickTaskWork) -> Self {
        Self { gate, callback }
    }

    pub(crate) fn enabled_generation(&self) -> Option<u64> {
        self.gate.enabled_generation()
    }

    pub(crate) fn generation_is_enabled(&self, generation: u64) -> bool {
        self.gate.generation_is_enabled(generation)
    }

    pub(crate) fn gate(&self) -> Arc<SchedulerTickGate> {
        Arc::clone(&self.gate)
    }

    pub(crate) unsafe fn invoke(&self, data: usize, thread: ThreadId) {
        unsafe { (self.callback)(data, thread) };
    }
}
